//! Aligned text tables for human output.

/// How a column is aligned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Align {
    Left,
    Right,
}

/// Lays out `rows` under `headers` (none when empty) in columns two spaces
/// apart, without trailing spaces. Columns without an entry in `align` are
/// left-aligned; widths count characters.
pub(crate) fn layout(headers: &[&str], rows: &[Vec<String>], align: &[Align]) -> Vec<String> {
    let header: Vec<String> = headers.iter().map(|h| (*h).to_owned()).collect();
    let all: Vec<&Vec<String>> = std::iter::once(&header)
        .filter(|h| !h.is_empty())
        .chain(rows.iter())
        .collect();
    let columns = all.iter().map(|r| r.len()).max().unwrap_or(0);
    let width = |c: usize| {
        all.iter()
            .filter_map(|r| r.get(c))
            .map(|cell| cell.chars().count())
            .max()
            .unwrap_or(0)
    };
    let widths: Vec<usize> = (0..columns).map(width).collect();
    all.iter()
        .map(|row| {
            let mut line = String::new();
            for (c, cell) in row.iter().enumerate() {
                if c > 0 {
                    line.push_str("  ");
                }
                let pad = widths[c].saturating_sub(cell.chars().count());
                match align.get(c).copied().unwrap_or(Align::Left) {
                    Align::Left => {
                        line.push_str(cell);
                        line.push_str(&" ".repeat(pad));
                    }
                    Align::Right => {
                        line.push_str(&" ".repeat(pad));
                        line.push_str(cell);
                    }
                }
            }
            line.trim_end().to_owned()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    fn row(cells: &[&str]) -> Vec<String> {
        cells.iter().map(|c| (*c).to_owned()).collect()
    }

    #[test]
    fn columns_line_up() {
        let lines = layout(
            &["CLIP", "TAKE", "IN"],
            &[
                row(&["c7f3", "take-0001", "0f"]),
                row(&["c8a1b", "take-0012", "455f"]),
            ],
            &[Align::Left, Align::Left, Align::Right],
        );
        assert_eq!(
            lines,
            [
                "CLIP   TAKE         IN",
                "c7f3   take-0001    0f",
                "c8a1b  take-0012  455f",
            ]
        );
        assert_eq!(
            layout(&[], &[row(&["a", ""]), row(&["bb", "c"])], &[]),
            ["a", "bb  c"]
        );
        assert!(layout(&[], &[], &[]).is_empty());
        assert_eq!(
            layout(&["Ü", "x"], &[row(&["ü1", "y"])], &[]),
            ["Ü   x", "ü1  y"]
        );
    }
}
