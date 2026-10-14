//! Small GStreamer helpers for the engine: checked element creation and
//! property setting (gstreamer-rs panics on unknown properties; plans are
//! data, so a bad value must become an error instead), state changes with
//! deadlines, and running times.

use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use gst::glib;
use gst::prelude::*;

/// Creates `factory` named `name`, or says which element is missing.
pub(super) fn make(factory: &str, name: &str) -> Result<gst::Element, String> {
    gst::ElementFactory::make(factory)
        .name(name)
        .build()
        .map_err(|_| {
            format!("the GStreamer element {factory} is missing (see `qrec doctor`)")
        })
}

/// Sets a property from its string form (ints, enums by nick, flags, bools).
pub(super) fn set_str(obj: &impl IsA<glib::Object>, name: &str, value: &str) -> Result<(), String> {
    let obj = obj.as_ref();
    let pspec = obj.find_property(name).ok_or_else(|| {
        format!("{} has no property {name}", obj.type_().name())
    })?;
    let value = glib::Value::deserialize_with_pspec(value, &pspec).map_err(|_| {
        format!("{}: invalid value {value:?} for {name}", obj.type_().name())
    })?;
    obj.set_property_from_value(name, &value);
    Ok(())
}

/// Sets a property from a typed value, converting it to the property's type.
pub(super) fn set_value(
    obj: &impl IsA<glib::Object>,
    name: &str,
    value: impl ToValue,
) -> Result<(), String> {
    let obj = obj.as_ref();
    let pspec = obj.find_property(name).ok_or_else(|| {
        format!("{} has no property {name}", obj.type_().name())
    })?;
    let value = value.to_value();
    let value = if value.type_().is_a(pspec.value_type()) {
        value
    } else {
        value.transform_with_type(pspec.value_type()).map_err(|_| {
            format!("{}: wrong value type for {name}", obj.type_().name())
        })?
    };
    obj.set_property_from_value(name, &value);
    Ok(())
}

/// Parses a caps string.
pub(super) fn caps(text: &str) -> Result<gst::Caps, String> {
    text.parse::<gst::Caps>()
        .map_err(|_| format!("invalid caps {text:?}"))
}

/// A `capsfilter` with `caps`.
pub(super) fn capsfilter(name: &str, text: &str) -> Result<gst::Element, String> {
    let filter = make("capsfilter", name)?;
    filter.set_property("caps", caps(text)?);
    Ok(filter)
}

/// Sets each element to NULL on a helper thread and waits at most
/// `timeout` for all of them. `false` = the change didn't finish in time
/// (a wedged driver); the helper is left behind and keeps its references.
pub(super) fn set_null_with_deadline(elements: Vec<gst::Element>, timeout: Duration) -> bool {
    if elements.is_empty() {
        return true;
    }
    let (tx, rx) = mpsc::channel();
    let spawned = thread::Builder::new()
        .name("qrec-gst-teardown".into())
        .spawn(move || {
            for element in &elements {
                let _ = element.set_state(gst::State::Null);
            }
            let _ = tx.send(());
        });
    match spawned {
        Ok(_) => rx.recv_timeout(timeout).is_ok(),
        Err(_) => false,
    }
}

/// `clock time − base time` of a playing element.
pub(super) fn running_time(element: &impl IsA<gst::Element>) -> Option<u64> {
    element.current_running_time().map(gst::ClockTime::nseconds)
}

/// `"<message> (<debug>)"` for logs; the message alone for people.
pub(super) fn error_text(err: &glib::Error, debug: Option<&str>) -> (String, String) {
    let message = err.message().to_owned();
    let detailed = match debug {
        Some(debug) if !debug.is_empty() => format!("{message} ({debug})"),
        _ => message.clone(),
    };
    (message, detailed)
}

/// The name of `obj` and of every parent, innermost first.
pub(super) fn object_path(obj: &gst::Object) -> Vec<String> {
    let mut names = Vec::new();
    let mut current = Some(obj.clone());
    while let Some(o) = current {
        names.push(o.name().to_string());
        current = o.parent();
    }
    names
}
