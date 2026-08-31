//! Help pager render loop
//!
//! This module contains the logic for rendering help content with overlays applied. Overlays are used for search matches,
//! mouse-hovered links, and link hints. The rendering process makes sure that the original content's structure is preserved
//! while applying the specified styles and inserts.

use std::ops::Range;

use yansi::Style;

use super::markup::{RESET_SEQ, scan_sgr};

#[derive(Debug, Clone)]
pub(super) enum Overlay {
  // used for search matches and mouse-hovered links
  // overrides a style within a certain range
  Span {
    range: Range<usize>,
    style: Style,
  },
  // used for link hints
  // inserts arbitrary styled text at a given position
  Insert {
    pos: usize,
    text: String,
    style: Style,
  },
}

/// Render `content` with `overlays` applied.
pub(super) fn render(content: &str, overlays: Vec<Overlay>) -> String {
  let mut events: Vec<(usize, EventKind)> = Vec::with_capacity(overlays.len() * 2);
  for ov in overlays {
    match ov {
      Overlay::Span { range, style } => {
        events.push((range.start, EventKind::OpenOverlay(style)));
        events.push((range.end, EventKind::CloseOverlay));
      }
      Overlay::Insert { pos, text, style } => {
        events.push((pos, EventKind::Insert { text, style }));
      }
    }
  }

  // enforce event precedence for overlays that land on the same byte position
  // close overlays > insert overlays > open overlays
  events.sort_by(|a, b| a.0.cmp(&b.0).then(event_rank(&a.1).cmp(&event_rank(&b.1))));

  let mut out = String::with_capacity(content.len() + events.len() * 16);
  let mut struct_stack: Vec<&str> = Vec::new();
  let mut overlay_stack: Vec<Style> = Vec::new();
  let mut events_iter = events.into_iter().peekable();
  let mut cursor = 0;

  let bytes = content.as_bytes();
  while cursor < bytes.len() {
    // Fire any events at the current cursor.
    drain_events_at(
      cursor,
      &mut events_iter,
      &mut out,
      &mut struct_stack,
      &mut overlay_stack,
    );

    match scan_sgr(bytes, cursor) {
      Some(seq_end) if &content[cursor..seq_end] == RESET_SEQ => {
        // pop the most recent style, then re-apply any overlays
        // that are still alive.
        let seq = &content[cursor..seq_end];
        struct_stack.pop();

        out.push_str(seq);
        reemit_overlays(&mut out, &overlay_stack);
        cursor = seq_end;
      }

      Some(seq_end) => {
        // push new style
        let seq = &content[cursor..seq_end];
        struct_stack.push(seq);

        out.push_str(seq);
        cursor = seq_end;
      }

      None => {
        // plain text, just copy until the next SGR sequence
        let next_event = events_iter.peek().map_or(bytes.len(), |(p, _)| *p);
        let next_sgr = find_sgr(bytes, cursor + 1).unwrap_or(bytes.len());
        let run_end = next_event.min(next_sgr).min(bytes.len()).max(cursor + 1);

        out.push_str(&content[cursor..run_end]);
        cursor = run_end;
      }
    }
  }

  // drain any events past end-of-content. Only Insert really makes sense
  // here, but drain Close/Open too just to be sure.
  drain_events_at(
    usize::MAX,
    &mut events_iter,
    &mut out,
    &mut struct_stack,
    &mut overlay_stack,
  );

  out
}

enum EventKind {
  OpenOverlay(Style),
  CloseOverlay,
  Insert { text: String, style: Style },
}

fn event_rank(ev: &EventKind) -> u8 {
  match ev {
    EventKind::CloseOverlay => 0,
    EventKind::Insert { .. } => 1,
    EventKind::OpenOverlay(_) => 2,
  }
}

fn drain_events_at<I>(
  up_to: usize,
  iter: &mut std::iter::Peekable<I>,
  out: &mut String,
  struct_stack: &mut Vec<&str>,
  overlay_stack: &mut Vec<Style>,
) where
  I: Iterator<Item = (usize, EventKind)>,
{
  while let Some((pos, _)) = iter.peek() {
    if *pos > up_to {
      break;
    }
    let (_, ev) = iter.next().unwrap();
    match ev {
      EventKind::OpenOverlay(style) => {
        emit_style_prefix(out, &style);
        overlay_stack.push(style);
      }
      EventKind::CloseOverlay => {
        overlay_stack.pop();
        out.push_str(RESET_SEQ);
        for seq in struct_stack.iter() {
          out.push_str(seq);
        }
        reemit_overlays(out, overlay_stack);
      }
      EventKind::Insert { text, style } => {
        emit_style_prefix(out, &style);
        out.push_str(&text);
        out.push_str(RESET_SEQ);
        for seq in struct_stack.iter() {
          out.push_str(seq);
        }
        reemit_overlays(out, overlay_stack);
      }
    }
  }
}

fn reemit_overlays(out: &mut String, overlay_stack: &[Style]) {
  for style in overlay_stack {
    emit_style_prefix(out, style);
  }
}

/// Emit just the SGR opening sequence for `style`, no trailing reset.
fn emit_style_prefix(out: &mut String, style: &Style) {
  let _ = style.fmt_prefix(out);
}

/// Find the next instance of `\x1b[`
fn find_sgr(bytes: &[u8], start: usize) -> Option<usize> {
  let pos = bytes[start..]
    .iter()
    .position(|b| *b == b'\x1b')
    .map(|p| start + p)?;

  (bytes.get(pos + 1)? == &b'[').then_some(pos)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::builtin::help::markup::{REF_SEQ, RESET_SEQ};

  #[test]
  fn render_passes_through_with_no_overlays() {
    let content = format!("see {REF_SEQ}autocmd{RESET_SEQ} for details");
    assert_eq!(render(&content, vec![]), content);
  }

  #[test]
  fn overlay_inside_structural_reapplies_outer() {
    // Cross-ref `autocmd`, search hit covers all of "autocmd". After the
    // search close, the outer REF_SEQ should be re-emitted so that the
    // following structural RESET still has something to close.
    let content = format!("{REF_SEQ}autocmd{RESET_SEQ}");
    let search = Style::new().bold().invert().cyan();
    let prefix_len = REF_SEQ.len();
    let overlays = vec![Overlay::Span {
      range: prefix_len..prefix_len + "autocmd".len(),
      style: search,
    }];
    let out = render(&content, overlays);
    // REF_SEQ, search open, "autocmd", reset, REF_SEQ (reapplied), reset.
    // We only assert the bracketing — the exact SGR bytes for `search`
    // come from yansi.
    assert!(out.starts_with(REF_SEQ));
    assert!(out.contains("autocmd"));
    let last_reset = out.rfind(RESET_SEQ).unwrap();
    let preceding = &out[..last_reset];
    // After the inner reset, REF_SEQ must appear again before the final
    // close — that's the re-emission.
    let inner_reset = preceding.rfind(RESET_SEQ).unwrap();
    assert!(
      preceding[inner_reset + RESET_SEQ.len()..].starts_with(REF_SEQ),
      "expected REF_SEQ reapplied after inner close in: {out:?}",
    );
  }

  #[test]
  fn insert_emits_text_and_resumes_outer() {
    let content = format!("{REF_SEQ}autocmd{RESET_SEQ}");
    let tag = Style::new().bold().yellow();
    let overlays = vec![Overlay::Insert {
      pos: REF_SEQ.len(),
      text: "[a]".into(),
      style: tag,
    }];
    let out = render(&content, overlays);
    // The visible characters should be `[a]autocmd` (the hint key followed
    // by the cross-ref content). Strip ANSI for the visual assertion.
    let visible = strip_ansi(&out);
    assert_eq!(visible, "[a]autocmd");
    // The hint key insert should not leave structural styling stripped:
    // REF_SEQ should be re-applied after the insert's reset.
    assert!(
      out.contains(&format!("{RESET_SEQ}{REF_SEQ}")),
      "REF_SEQ should resume after the insert: {out:?}"
    );
  }

  #[test]
  fn newline_count_preserved_with_overlays() {
    // The pager assumes `render(...)` doesn't change the line count of the
    // input — every `\n` in content corresponds to exactly one displayed
    // row. Overlays must not inject or strip newlines.
    let content = format!("line one\nline two with {REF_SEQ}autocmd{RESET_SEQ} link\nline three\n");
    let overlays = vec![
      Overlay::Span {
        range: 25..32, // arbitrary search hit, doesn't matter exactly where
        style: Style::new().bold().invert(),
      },
      Overlay::Insert {
        pos: 9,
        text: "[a]".into(),
        style: Style::new().bold().yellow(),
      },
    ];
    let out = render(&content, overlays);
    let in_newlines = content.bytes().filter(|b| *b == b'\n').count();
    let out_newlines = out.bytes().filter(|b| *b == b'\n').count();
    assert_eq!(
      in_newlines, out_newlines,
      "render must preserve newline count; in={in_newlines} out={out_newlines}\nout={out:?}"
    );
  }

  #[test]
  fn overlays_outside_structural_just_paint() {
    let content = "plain text".to_string();
    let style = Style::new().bold();
    let overlays = vec![Overlay::Span { range: 0..5, style }];
    let out = render(&content, overlays);
    assert_eq!(strip_ansi(&out), "plain text");
  }

  fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
      if bytes[i] == 0x1b
        && let Some(end) = scan_sgr(bytes, i)
      {
        i = end;
        continue;
      }
      out.push(bytes[i] as char);
      i += 1;
    }
    out
  }
}
