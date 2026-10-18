//! Edits of a small TOML file as text (A§9.7, A§9.8): one value replaced,
//! one `key = value` line added or removed, and every other byte left as
//! it was: the comments, the blank lines, the order and the spelling of
//! each line. `toml_edit`'s documents keep comments, but print a dotted
//! table's keys together (`ui.scale`, `canvas.fps`, `ui.sounds` come back
//! with the two `ui.` lines next to each other), so these edits cut the
//! text at the spans its parser reports instead. A layout they can't edit
//! as text (a line to add to or remove from an inline table) answers
//! `None`, and the caller edits a `toml_edit` document instead. A line
//! these edits add ends as the file's lines do ([`newline`]: CRLF in a
//! file a Windows editor saved), and so does a document edit's text
//! ([`with_newlines_of`]). Owner: meta (for `config::set_text` and
//! `keymap::set_binding`).

use std::ops::Range;

use toml_edit::{Document, Item, Table, TableLike, Value};

/// One `key = value` of a document (a value that isn't a table).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Entry {
    /// Its keys from the root: `["ui", "scale"]` for `[ui] scale = 1` and
    /// for `ui.scale = 1`; a quoted key is one name (`["bindings",
    /// "clip.split"]` for `"clip.split" = "s"` under `[bindings]`).
    pub path: Vec<String>,
    /// Where its key starts.
    pub key_start: usize,
    /// Its value (a multi-line array spans lines).
    pub value: Range<usize>,
    /// It sits in an inline table (`ui = { scale = 1 }`): no line of its own.
    pub inline: bool,
}

/// Parses `text`, keeping the spans.
pub(crate) fn parse(text: &str) -> Result<Document<String>, toml_edit::TomlError> {
    Document::parse(text.to_owned())
}

/// Every `key = value` of `doc`, in no particular order.
pub(crate) fn entries(doc: &Document<String>) -> Vec<Entry> {
    let mut out = Vec::new();
    walk(doc.as_table(), &mut Vec::new(), false, &mut out);
    out
}

fn walk(table: &dyn TableLike, path: &mut Vec<String>, inline: bool, out: &mut Vec<Entry>) {
    for (name, item) in table.iter() {
        path.push(name.to_owned());
        match item {
            Item::Table(t) => walk(t, path, inline, out),
            Item::Value(Value::InlineTable(t)) => walk(t, path, true, out),
            Item::Value(v) => {
                let key_start = table.key(name).and_then(|k| k.span()).map(|s| s.start);
                if let (Some(key_start), Some(value)) = (key_start, v.span()) {
                    out.push(Entry {
                        path: path.clone(),
                        key_start,
                        value,
                        inline,
                    });
                }
            }
            Item::None | Item::ArrayOfTables(_) => {}
        }
        path.pop();
    }
}

/// The byte where the line holding `at` starts.
pub(crate) fn line_start(text: &str, at: usize) -> usize {
    text[..at].rfind('\n').map_or(0, |i| i + 1)
}

/// The byte after the newline that ends the line holding `at` (the end of
/// the text on its last line).
pub(crate) fn line_end(text: &str, at: usize) -> usize {
    text[at..].find('\n').map_or(text.len(), |i| at + i + 1)
}

/// `text` with `entry`'s value replaced by `value` (TOML text): its key,
/// its spacing and a comment after it stay.
pub(crate) fn replace_value(text: &str, entry: &Entry, value: &str) -> String {
    format!(
        "{}{value}{}",
        &text[..entry.value.start],
        &text[entry.value.end..]
    )
}

/// `text` without the lines of `entries` (a comment at the end of one goes
/// with it; the lines around them stay); `None` when one is in an inline
/// table.
pub(crate) fn remove_lines(text: &str, entries: &[&Entry]) -> Option<String> {
    if entries.iter().any(|e| e.inline) {
        return None;
    }
    let mut cuts: Vec<Range<usize>> = entries
        .iter()
        .map(|e| line_start(text, e.key_start)..line_end(text, e.value.end))
        .collect();
    cuts.sort_by_key(|r| r.start);
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for cut in cuts {
        if cut.start < at {
            // Two entries on one line can only be in an inline table.
            return None;
        }
        out.push_str(&text[at..cut.start]);
        at = cut.end;
    }
    out.push_str(&text[at..]);
    Some(out)
}

/// `key` (one name) as TOML writes it: bare when it can be, else quoted.
pub(crate) fn key_text(key: &str) -> String {
    let bare = !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if bare {
        key.to_owned()
    } else {
        toml_edit::Key::new(key).display_repr().into_owned()
    }
}

/// How a document writes the table at a path of keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TableForm {
    /// The document itself.
    Root,
    /// `[ui]`, or a table only its sub-tables' headers name
    /// (`[bindings.view]` alone names `bindings`).
    Header,
    /// Dotted keys of the table above (`ui.scale = 1` at the root).
    Dotted,
    /// No table has that name.
    Missing,
    /// An inline table, or a value where a table would be.
    Other,
}

/// How `doc` writes the table at `path` (`[]`: the root).
pub(crate) fn table_form(doc: &Document<String>, path: &[&str]) -> TableForm {
    match table_at(doc.as_table(), path) {
        Found::Root => TableForm::Root,
        Found::Table(t) if t.is_dotted() => TableForm::Dotted,
        Found::Table(_) => TableForm::Header,
        Found::Missing => TableForm::Missing,
        Found::Unusable => TableForm::Other,
    }
}

/// `text` with the line `line` (`key = value`, no newline) added to the
/// table `table` (`[]`: the root), whose [`table_form`] is `Root`,
/// `Header` or `Missing`: after the last of that table's own lines, under
/// its header when it has none, or in a new `[table]` at the end of the
/// text when no header names it. A root line in a document without one
/// goes above the first header and the comments on top of it. `None` for
/// the other forms: a dotted table's lines go to the table above, an
/// inline table's can't be added as text.
pub(crate) fn add_line(
    text: &str,
    doc: &Document<String>,
    table: &[&str],
    line: &str,
) -> Option<String> {
    let root_first = table.is_empty() && body_end(doc.as_table()).is_none();
    let at = match table_at(doc.as_table(), table) {
        Found::Root => match body_end(doc.as_table()) {
            Some(end) => line_end(text, end),
            None => before_first_header(text, doc),
        },
        Found::Table(t) if t.is_dotted() => return None,
        Found::Table(t) => match (body_end(t), t.span()) {
            (Some(end), _) => line_end(text, end),
            (None, Some(header)) if !t.is_implicit() => line_end(text, header.end),
            // Named only by its sub-tables' headers: a header of its own
            // (TOML allows it after them).
            _ => return Some(with_new_table(text, table, line)),
        },
        Found::Missing => return Some(with_new_table(text, table, line)),
        Found::Unusable => return None,
    };
    let newline = newline(text);
    let mut out = String::with_capacity(text.len() + line.len() + 4);
    out.push_str(&text[..at]);
    if !out.is_empty() && !out.ends_with('\n') {
        out.push_str(newline);
    }
    out.push_str(line);
    out.push_str(newline);
    // A root line added above a header's comments keeps a blank line
    // between them.
    if at < text.len() && root_first {
        out.push_str(newline);
    }
    out.push_str(&text[at..]);
    Some(out)
}

/// The line ending `text` uses, its first line's: `"\r\n"` (a Windows
/// editor's file) or `"\n"` (one line, or none, is LF's).
pub(crate) fn newline(text: &str) -> &'static str {
    match text.find('\n') {
        Some(i) if text[..i].ends_with('\r') => "\r\n",
        _ => "\n",
    }
}

/// `text` ends with a blank line: a TOML table is set off from what is
/// above it by one.
fn ends_with_blank_line(text: &str) -> bool {
    text.ends_with("\n\n") || text.ends_with("\n\r\n")
}

/// What a table path names in a document.
enum Found<'a> {
    Root,
    Table(&'a Table),
    Missing,
    /// An inline table, or a value where a table should be.
    Unusable,
}

fn table_at<'a>(root: &'a Table, path: &[&str]) -> Found<'a> {
    if path.is_empty() {
        return Found::Root;
    }
    let mut table = root;
    for name in path {
        match table.get(name) {
            None => return Found::Missing,
            Some(Item::Table(t)) => table = t,
            Some(_) => return Found::Unusable,
        }
    }
    Found::Table(table)
}

/// The end of the last value written in `table`'s own lines: its values
/// (an inline table whole) and those of its dotted tables, not those under
/// a header of their own.
fn body_end(table: &Table) -> Option<usize> {
    table
        .iter()
        .filter_map(|(_, item)| match item {
            Item::Value(v) => v.span().map(|s| s.end),
            Item::Table(t) if t.is_dotted() => body_end(t),
            _ => None,
        })
        .max()
}

/// Where a root line goes in a document without one: above the first
/// header and the comments right on top of it (they belong to it), or at
/// the end.
fn before_first_header(text: &str, doc: &Document<String>) -> usize {
    let mut first: Option<usize> = None;
    headers(doc.as_table(), &mut |start| {
        first = Some(first.map_or(start, |f: usize| f.min(start)));
    });
    let Some(header) = first else {
        return text.len();
    };
    let mut at = line_start(text, header);
    while at > 0 {
        let prev = line_start(text, at - 1);
        if text[prev..at].trim_start().starts_with('#') {
            at = prev;
        } else {
            break;
        }
    }
    at
}

fn headers(table: &Table, found: &mut dyn FnMut(usize)) {
    for (_, item) in table.iter() {
        if let Item::Table(t) = item {
            if !t.is_dotted()
                && let Some(span) = t.span()
            {
                found(span.start);
            }
            headers(t, found);
        }
    }
}

/// `edited` (a `toml_edit` document's text, the fallback edits') with
/// `original`'s line endings: in a CRLF file every LF that isn't one
/// already becomes CRLF.
pub(crate) fn with_newlines_of(original: &str, edited: String) -> String {
    if newline(original) == "\n" {
        return edited;
    }
    let mut out = String::with_capacity(edited.len() + edited.len() / 16);
    let mut previous = None;
    for c in edited.chars() {
        if c == '\n' && previous != Some('\r') {
            out.push('\r');
        }
        out.push(c);
        previous = Some(c);
    }
    out
}

/// `text` with `[table]` and `line` added at its end, in `text`'s line
/// endings.
fn with_new_table(text: &str, table: &[&str], line: &str) -> String {
    let newline = newline(text);
    let mut out = text.to_owned();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push_str(newline);
    }
    if !out.is_empty() && !ends_with_blank_line(&out) {
        out.push_str(newline);
    }
    let header: Vec<String> = table.iter().map(|t| key_text(t)).collect();
    out.push_str(&format!("[{}]{newline}{line}{newline}", header.join(".")));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry<'a>(entries: &'a [Entry], path: &[&str]) -> &'a Entry {
        entries
            .iter()
            .find(|e| e.path == path)
            .unwrap_or_else(|| panic!("no {path:?} in {entries:?}"))
    }

    const KEYS: &str = "\
# Qrec keys
[bindings]
# Output
frame.save = \"f\"   # closer to my left hand
# Edit
clip.split = \"s\"
\"clip.remove\" = [
  \"Delete\",
]
clip.trim-start = \"q\" # trailing

[bindings.view]
zoom = 1
";

    #[test]
    fn entries_name_every_value_by_its_keys() {
        let doc = parse(KEYS).unwrap();
        let all = entries(&doc);
        let mut paths: Vec<String> = all.iter().map(|e| e.path.join("/")).collect();
        paths.sort();
        assert_eq!(
            paths,
            [
                "bindings/clip.remove",
                "bindings/clip/split",
                "bindings/clip/trim-start",
                "bindings/frame/save",
                "bindings/view/zoom"
            ]
        );
        let save = entry(&all, &["bindings", "frame", "save"]);
        assert_eq!(&KEYS[save.value.clone()], "\"f\"");
        assert_eq!(
            &KEYS[line_start(KEYS, save.key_start)..line_end(KEYS, save.value.end)],
            "frame.save = \"f\"   # closer to my left hand\n"
        );
        let remove = entry(&all, &["bindings", "clip.remove"]);
        assert_eq!(&KEYS[remove.value.clone()], "[\n  \"Delete\",\n]");
        let inline = parse("ui = { scale = 1.5 }\n").unwrap();
        assert!(entries(&inline)[0].inline);
    }

    #[test]
    fn a_value_is_replaced_in_place() {
        let doc = parse(KEYS).unwrap();
        let all = entries(&doc);
        let edited = replace_value(KEYS, entry(&all, &["bindings", "frame", "save"]), "\"g\"");
        assert_eq!(
            edited,
            KEYS.replace("frame.save = \"f\"", "frame.save = \"g\"")
        );
    }

    #[test]
    fn lines_are_removed_with_their_trailing_comment_only() {
        let doc = parse(KEYS).unwrap();
        let all = entries(&doc);
        let edited = remove_lines(
            KEYS,
            &[
                entry(&all, &["bindings", "clip.remove"]),
                entry(&all, &["bindings", "frame", "save"]),
            ],
        )
        .unwrap();
        assert_eq!(
            edited,
            "# Qrec keys\n[bindings]\n# Output\n# Edit\nclip.split = \"s\"\n\
             clip.trim-start = \"q\" # trailing\n\n[bindings.view]\nzoom = 1\n"
        );
        let inline = "ui = { scale = 1.5 }\n";
        let doc = parse(inline).unwrap();
        assert_eq!(remove_lines(inline, &[&entries(&doc)[0]]), None);
    }

    #[test]
    fn a_line_goes_after_its_tables_last_line() {
        let doc = parse(KEYS).unwrap();
        let edited = add_line(KEYS, &doc, &["bindings"], "export = \"F5\"").unwrap();
        assert_eq!(
            edited,
            KEYS.replace(
                "clip.trim-start = \"q\" # trailing\n",
                "clip.trim-start = \"q\" # trailing\nexport = \"F5\"\n"
            )
        );
        // Under the header of a table with no lines yet.
        let text = "# mine\n[bindings]\n\n[other]\na = 1\n";
        let doc = parse(text).unwrap();
        assert_eq!(
            add_line(text, &doc, &["bindings"], "x = 1").unwrap(),
            "# mine\n[bindings]\nx = 1\n\n[other]\na = 1\n"
        );
        // A table no header names: a new one at the end.
        let text = "theme = \"swamp-day\"";
        let doc = parse(text).unwrap();
        assert_eq!(
            add_line(text, &doc, &["ui"], "scale = 1.5").unwrap(),
            "theme = \"swamp-day\"\n\n[ui]\nscale = 1.5\n"
        );
        let doc = parse("").unwrap();
        assert_eq!(
            add_line("", &doc, &["bindings"], "x = 1").unwrap(),
            "[bindings]\nx = 1\n"
        );
        // Dotted at the root: a root line, spelled like the others.
        let text = "ui.scale = 1.25\ncanvas.fps = \"30\"\n";
        let doc = parse(text).unwrap();
        assert_eq!(table_form(&doc, &["ui"]), TableForm::Dotted);
        assert_eq!(add_line(text, &doc, &["ui"], "sounds = true"), None);
        assert_eq!(
            add_line(text, &doc, &[], "ui.sounds = true").unwrap(),
            "ui.scale = 1.25\ncanvas.fps = \"30\"\nui.sounds = true\n"
        );
        // Named only by a sub-table's header: a header of its own, after it.
        let text = "[bindings.view]\nzoom = 1\n";
        let doc = parse(text).unwrap();
        assert_eq!(table_form(&doc, &["bindings"]), TableForm::Header);
        let edited = add_line(text, &doc, &["bindings"], "x = 1").unwrap();
        assert_eq!(edited, "[bindings.view]\nzoom = 1\n\n[bindings]\nx = 1\n");
        assert!(parse(&edited).is_ok(), "{edited}");
        // Inline: not as text.
        let text = "ui = { scale = 1.5 }\n";
        let doc = parse(text).unwrap();
        assert_eq!(table_form(&doc, &["ui"]), TableForm::Other);
        assert_eq!(add_line(text, &doc, &["ui"], "sounds = true"), None);
        assert_eq!(table_form(&doc, &["nope"]), TableForm::Missing);
        assert_eq!(table_form(&doc, &[]), TableForm::Root);
    }

    #[test]
    fn a_root_line_goes_above_the_first_headers_comments() {
        let text = "# Qrec settings\n\n# lower = more sensitive\n[autocut]\nthreshold_db = -35.0\n";
        let doc = parse(text).unwrap();
        assert_eq!(
            add_line(text, &doc, &[], "theme = \"ogre-mode\"").unwrap(),
            "# Qrec settings\n\ntheme = \"ogre-mode\"\n\n# lower = more sensitive\n[autocut]\nthreshold_db = -35.0\n"
        );
        let text = "theme = \"swamp-day\" # day\n\n[ui]\nscale = 1.0\n";
        let doc = parse(text).unwrap();
        assert_eq!(
            add_line(text, &doc, &[], "x = 1").unwrap(),
            "theme = \"swamp-day\" # day\nx = 1\n\n[ui]\nscale = 1.0\n"
        );
    }

    /// A file with CRLF line endings (a Windows editor's) gets CRLF lines:
    /// a line in a table, a new table at the end (after one blank line,
    /// as in an LF file), a root line above the first header's comments.
    #[test]
    fn a_crlf_file_gets_crlf_lines() {
        let text = "# mine\r\n[bindings]\r\nclip.split = \"s\"\r\n\r\n[other]\r\na = 1\r\n";
        let doc = parse(text).unwrap();
        assert_eq!(
            add_line(text, &doc, &["bindings"], "x = 1").unwrap(),
            "# mine\r\n[bindings]\r\nclip.split = \"s\"\r\nx = 1\r\n\r\n[other]\r\na = 1\r\n"
        );
        for (text, edited) in [
            (
                "theme = \"swamp-day\"\r\n",
                "theme = \"swamp-day\"\r\n\r\n[ui]\r\nscale = 1.5\r\n",
            ),
            (
                "theme = \"swamp-day\"\r\n\r\n",
                "theme = \"swamp-day\"\r\n\r\n[ui]\r\nscale = 1.5\r\n",
            ),
            (
                "# mine\r\ntheme = \"swamp-day\"",
                "# mine\r\ntheme = \"swamp-day\"\r\n\r\n[ui]\r\nscale = 1.5\r\n",
            ),
        ] {
            let doc = parse(text).unwrap();
            assert_eq!(
                add_line(text, &doc, &["ui"], "scale = 1.5").unwrap(),
                edited,
                "{text:?}"
            );
        }
        let text = "theme = \"swamp-day\"\r\nx = 1";
        let doc = parse(text).unwrap();
        assert_eq!(
            add_line(text, &doc, &[], "y = 2").unwrap(),
            "theme = \"swamp-day\"\r\nx = 1\r\ny = 2\r\n"
        );
        let text = "# Qrec settings\r\n\r\n# lower = more sensitive\r\n[autocut]\r\nthreshold_db = -35.0\r\n";
        let doc = parse(text).unwrap();
        assert_eq!(
            add_line(text, &doc, &[], "theme = \"ogre-mode\"").unwrap(),
            "# Qrec settings\r\n\r\ntheme = \"ogre-mode\"\r\n\r\n# lower = more sensitive\r\n\
             [autocut]\r\nthreshold_db = -35.0\r\n"
        );
        // A line goes with its CRLF.
        let text = "[bindings]\r\nx = 1\r\ny = 2\r\n";
        let doc = parse(text).unwrap();
        let all = entries(&doc);
        assert_eq!(
            remove_lines(text, &[entry(&all, &["bindings", "x"])]).unwrap(),
            "[bindings]\r\ny = 2\r\n"
        );
        // A document's text gets the original's line endings.
        assert_eq!(
            with_newlines_of(text, "[bindings]\ny = 2\r\nz = { a = 1 }\n".to_owned()),
            "[bindings]\r\ny = 2\r\nz = { a = 1 }\r\n"
        );
        assert_eq!(
            with_newlines_of("a = 1\n", "a = 1\nb = 2\n".to_owned()),
            "a = 1\nb = 2\n"
        );
    }

    #[test]
    fn keys_are_bare_when_they_can_be() {
        assert_eq!(key_text("trim-start"), "trim-start");
        assert_eq!(key_text("clip.split"), "\"clip.split\"");
        assert_eq!(key_text("a b"), "\"a b\"");
    }
}
