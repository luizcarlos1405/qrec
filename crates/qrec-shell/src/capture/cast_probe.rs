//! A manual probe of a real screencast (never run by default): opens one
//! portal session (its restore token makes it silent), dumps the node's
//! formats with `pw-dump`, then runs each candidate pipeline for a few
//! seconds and reports the frames that reached the sink.
//!
//! It found how niri's screencasts must be read (A§8.11, `dmabuf`):
//!
//! ```sh
//! QREC_PROBE_OUTPUT=DP-4 QREC_PROBE_TOKENS=~/.local/state/qrec/portal-tokens.json \
//! QREC_PROBE_DUMP=tmp/pw-dump.json QREC_PROBE_FIXMEM=1 QREC_PROBE_CPUREAD=1 \
//! QREC_PROBE_CF_CAPS='video/x-raw(memory:DMABuf),format=DMA_DRM,drm-format=XR24' \
//! QREC_PROBE_PIPES='capsfilter name=cf ;; videoconvert' \
//! cargo test -p qrec-shell --lib cast_probe -- --ignored --nocapture
//! ```
//!
//! Pipelines are separated by `;;`; each runs as `pipewiresrc … ! <it> !
//! fakesink`. A capsfilter named `cf` gets `QREC_PROBE_CF_CAPS` after
//! linking (DMA_DRM caps fail the link-time checks).

use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use gst::prelude::*;

use super::engine::Cmd;
use super::portal::{OpenRequest, PortalWorker};

#[test]
#[ignore = "opens a real screen-share portal session"]
fn cast_probe() {
    let var = |k: &str| std::env::var(k).ok();
    let output = var("QREC_PROBE_OUTPUT").unwrap_or_else(|| "DP-4".into());
    let tokens = PathBuf::from(var("QREC_PROBE_TOKENS").expect("QREC_PROBE_TOKENS"));
    let secs: u64 = var("QREC_PROBE_SECS")
        .and_then(|s| s.parse().ok())
        .unwrap_or(4);
    gst::init().unwrap();

    let worker = PortalWorker::spawn().unwrap();
    let (tx, rx) = crossbeam_channel::unbounded();
    assert!(worker.open(
        OpenRequest {
            id: 1,
            outputs: vec![output.clone()],
            screens: vec![],
            tokens,
            timeout: Duration::from_secs(120),
            register_app_id: false,
        },
        tx,
    ));
    let cast = match rx.recv_timeout(Duration::from_secs(130)).unwrap() {
        Cmd::PortalDone { mut results, .. } => results.remove(0).1.expect("the cast opened"),
        _ => panic!("unexpected reply"),
    };
    println!("cast {output}: node {} size {:?}", cast.node_id, cast.size);

    if let Some(path) = var("QREC_PROBE_DUMP") {
        let dump = std::process::Command::new("pw-dump").output().unwrap();
        std::fs::write(&path, &dump.stdout).unwrap();
        println!("pw-dump written to {path}");
    }

    for desc in var("QREC_PROBE_PIPES").unwrap_or_default().split(";;") {
        let desc = desc.trim();
        if desc.is_empty() {
            continue;
        }
        let full = format!(
            "pipewiresrc name=src fd={} path={} do-timestamp=true ! {desc} ! fakesink name=sink sync=false signal-handoffs=true",
            cast.fd_raw(),
            cast.node_id
        );
        println!("\n=== {desc}");
        let pipeline = match gst::parse::launch(&full) {
            Ok(p) => p.downcast::<gst::Pipeline>().unwrap(),
            Err(e) => {
                println!("  can't build: {e}");
                continue;
            }
        };
        // A capsfilter named `cf` gets QREC_PROBE_CF_CAPS only now, after
        // linking (a DMA_DRM filter fails the parse-time link checks).
        if let (Some(cf), Some(caps)) = (pipeline.by_name("cf"), var("QREC_PROBE_CF_CAPS")) {
            cf.set_property("caps", gst::Caps::from_str(&caps).unwrap());
        }
        let frames = Arc::new(AtomicU64::new(0));
        let caps = Arc::new(std::sync::Mutex::new(String::new()));
        let sink = pipeline.by_name("sink").unwrap();
        {
            let frames = frames.clone();
            sink.connect("handoff", false, move |_| {
                frames.fetch_add(1, Ordering::Relaxed);
                None
            });
        }
        if let Some(pad) = pipeline.by_name("src").and_then(|s| s.static_pad("src")) {
            let caps = caps.clone();
            pad.connect_notify(Some("caps"), move |pad, _| {
                if let Some(c) = pad.current_caps() {
                    *caps.lock().unwrap() = c.to_string();
                }
            });
            // The first buffer's video meta and memories. QREC_PROBE_FIXMEM
            // sizes dmabuf memories (`bridge::resize_dmabufs`), and
            // QREC_PROBE_CPUREAD maps the first frames and times a copy.
            let fixmem = var("QREC_PROBE_FIXMEM").is_some();
            let cpuread = var("QREC_PROBE_CPUREAD").is_some();
            let allocator = gst_allocators::DmaBufAllocator::new();
            let seen = Arc::new(AtomicU64::new(0));
            pad.add_probe(gst::PadProbeType::BUFFER, move |_pad, info| {
                if let Some(gst::PadProbeData::Buffer(buffer)) = &mut info.data {
                    let n = seen.fetch_add(1, Ordering::Relaxed);
                    if n == 0 {
                        let mems: Vec<String> = (0..buffer.n_memory())
                            .map(|i| {
                                let m = buffer.peek_memory(i);
                                let kind = m.allocator().map(|a| a.type_().name().to_string());
                                format!("{kind:?}@{}+{} max {}", m.offset(), m.size(), m.maxsize())
                            })
                            .collect();
                        match buffer.meta::<gst_video::VideoMeta>() {
                            Some(meta) => println!(
                                "  meta: format {:?} {}x{} planes {} offsets {:?} strides {:?}; memories {mems:?}",
                                meta.format(),
                                meta.width(),
                                meta.height(),
                                meta.n_planes(),
                                meta.offset(),
                                meta.stride(),
                            ),
                            None => println!("  meta: none; memories {mems:?}"),
                        }
                    }
                    if fixmem
                        && let Err(e) = super::bridge::resize_dmabufs(buffer.make_mut(), &allocator)
                    {
                        println!("  resize: {e}");
                    }
                    if cpuread && n < 5 {
                        let frame_bytes = buffer
                            .meta::<gst_video::VideoMeta>()
                            .map(|m| m.stride()[0] as usize * m.height() as usize);
                        match (buffer.map_readable(), frame_bytes) {
                            (Ok(map), Some(len)) if map.len() >= len => {
                                let t = Instant::now();
                                let copy = map.as_slice()[..len].to_vec();
                                let elapsed = t.elapsed();
                                let step = (len / 4096).max(1);
                                let samples: Vec<u8> = copy.iter().step_by(step).copied().collect();
                                let mean = samples.iter().map(|&b| u64::from(b)).sum::<u64>() as f64
                                    / samples.len().max(1) as f64;
                                let nonzero = samples.iter().filter(|&&b| b != 0).count();
                                println!(
                                    "  cpu read #{n}: {len} bytes copied in {elapsed:?}, mean {mean:.1}, nonzero {nonzero}/{}",
                                    samples.len()
                                );
                            }
                            (Ok(map), _) => println!("  cpu read #{n}: only {} bytes mapped", map.len()),
                            (Err(e), _) => println!("  cpu read #{n}: can't map: {e}"),
                        }
                    }
                }
                gst::PadProbeReturn::Ok
            });
        }
        if let Err(e) = pipeline.set_state(gst::State::Playing) {
            println!("  set_state: {e}");
        }
        let bus = pipeline.bus().unwrap();
        let until = Instant::now() + Duration::from_secs(secs);
        let mut error = None;
        while Instant::now() < until {
            if let Some(msg) = bus.timed_pop(gst::ClockTime::from_mseconds(100))
                && let gst::MessageView::Error(e) = msg.view()
            {
                error = Some(format!("{} ({:?})", e.error(), e.debug()));
                break;
            }
        }
        let _ = pipeline.set_state(gst::State::Null);
        println!("  source caps: {}", caps.lock().unwrap());
        println!(
            "  frames in {secs} s: {}{}",
            frames.load(Ordering::Relaxed),
            error.map(|e| format!("; error: {e}")).unwrap_or_default()
        );
    }
    worker.close(vec![cast]);
    std::thread::sleep(Duration::from_millis(500));
}
