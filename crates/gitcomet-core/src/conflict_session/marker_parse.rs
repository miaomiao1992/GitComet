use super::{ConflictRegion, ConflictRegionResolution};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsedConflictBlock {
    pub base: Option<String>,
    pub ours: String,
    pub theirs: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParsedConflictSegment {
    Text(String),
    Conflict(ParsedConflictBlock),
}

/// Parse merged text into alternating context text and conflict blocks.
///
/// Parsing is intentionally conservative. If a marker block is malformed,
/// all consumed marker text is preserved as context and parsing stops.
pub fn parse_conflict_marker_segments(text: &str) -> Vec<ParsedConflictSegment> {
    let mut segments = Vec::new();
    let mut context = String::new();
    let mut it = text.split_inclusive('\n').peekable();

    while let Some(line) = it.next() {
        if !line.starts_with("<<<<<<<") {
            context.push_str(line);
            continue;
        }

        // Flush context before the conflict block.
        if !context.is_empty() {
            segments.push(ParsedConflictSegment::Text(std::mem::take(&mut context)));
        }

        let mut base_marker_line: Option<&str> = None;
        let mut base: Option<String> = None;
        let mut ours = String::new();
        let mut found_sep = false;
        let mut separator_line: Option<String> = None;

        while let Some(l) = it.next() {
            if l.starts_with("=======") {
                found_sep = true;
                separator_line = Some(l.to_string());
                break;
            }
            if l.starts_with("|||||||") {
                base_marker_line = Some(l);
                let mut base_buf = String::new();
                for base_line in it.by_ref() {
                    if base_line.starts_with("=======") {
                        found_sep = true;
                        separator_line = Some(base_line.to_string());
                        break;
                    }
                    base_buf.push_str(base_line);
                }
                base = Some(base_buf);
                break;
            }
            ours.push_str(l);
        }

        if !found_sep {
            // Malformed: preserve all consumed content as context text.
            context.push_str(line);
            context.push_str(&ours);
            if let Some(base_marker_line) = base_marker_line {
                context.push_str(base_marker_line);
            }
            if let Some(ref base_content) = base {
                context.push_str(base_content);
            }
            break;
        }

        let mut theirs = String::new();
        let mut found_end = false;
        for l in it.by_ref() {
            if l.starts_with(">>>>>>>") {
                found_end = true;
                break;
            }
            theirs.push_str(l);
        }

        if !found_end {
            // Malformed: preserve all consumed content as context text.
            context.push_str(line);
            context.push_str(&ours);
            if let Some(base_marker_line) = base_marker_line {
                context.push_str(base_marker_line);
            }
            if let Some(ref base_content) = base {
                context.push_str(base_content);
            }
            if let Some(ref sep) = separator_line {
                context.push_str(sep);
            }
            context.push_str(&theirs);
            break;
        }

        segments.push(ParsedConflictSegment::Conflict(ParsedConflictBlock {
            base,
            ours,
            theirs,
        }));
    }

    // Flush trailing context.
    if !context.is_empty() {
        segments.push(ParsedConflictSegment::Text(context));
    }

    segments
}

/// Parse conflict marker blocks from merged text into conflict regions.
///
/// This is a thin wrapper over [`parse_conflict_marker_segments`] that
/// discards context text and keeps only conflict blocks.
pub(super) fn parse_conflict_regions_from_markers(text: &str) -> Vec<ConflictRegion> {
    parse_conflict_marker_segments(text)
        .into_iter()
        .filter_map(|segment| match segment {
            ParsedConflictSegment::Text(_) => None,
            ParsedConflictSegment::Conflict(block) => Some(ConflictRegion {
                base: block.base,
                ours: block.ours,
                theirs: block.theirs,
                resolution: ConflictRegionResolution::Unresolved,
            }),
        })
        .collect()
}
