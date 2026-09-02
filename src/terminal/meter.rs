use crate::{
    obs::{MicrophoneLevel, MicrophoneState},
    theme::Palette,
};
use ratatui::{style::Style, text::Span};

pub(super) struct MeterLine {
    pub(super) spans: Vec<Span<'static>>,
    pub(super) width: usize,
}

pub(super) fn microphone_meter(
    state: MicrophoneState,
    level: Option<MicrophoneLevel>,
    available: usize,
    palette: Palette,
) -> Option<MeterLine> {
    if available < 10 || state == MicrophoneState::Unknown {
        return None;
    }

    if state == MicrophoneState::Muted {
        return Some(MeterLine {
            spans: vec![Span::styled(
                " MIC MUTE ",
                Style::default().fg(palette.warning),
            )],
            width: 10,
        });
    }

    let target_width = available.min(24);
    let Some(level) = level else {
        return Some(MeterLine {
            spans: vec![Span::styled(
                " MIC  -- ",
                Style::default().fg(palette.frame),
            )],
            width: 10,
        });
    };

    let full = target_width >= 18;
    let bar_width = if full {
        target_width.saturating_sub(9)
    } else {
        target_width.saturating_sub(5)
    };
    let mut spans = Vec::with_capacity(7);
    spans.push(Span::styled(" MIC ", Style::default().fg(palette.content)));
    push_bar(&mut spans, level.peak_db, bar_width, palette);
    if full {
        spans.push(Span::styled(
            format!(" {:>3}", level.peak_db.round() as i16),
            Style::default().fg(palette.content),
        ));
    }
    Some(MeterLine {
        spans,
        width: target_width,
    })
}

fn push_bar(spans: &mut Vec<Span<'static>>, db: f32, width: usize, palette: Palette) {
    let normalized = ((db.clamp(-60.0, 0.0) + 60.0) / 60.0).clamp(0.0, 1.0);
    let filled = (normalized * width as f32).round() as usize;
    let green_end = width * 40 / 60;
    let yellow_end = width * 51 / 60;
    let green = filled.min(green_end);
    let yellow = filled.saturating_sub(green).min(yellow_end - green_end);
    let red = filled.saturating_sub(green + yellow);
    let empty = width.saturating_sub(filled);

    push_segment(spans, "━", green, palette.success);
    push_segment(spans, "━", yellow, palette.rank);
    push_segment(spans, "━", red, palette.warning);
    push_segment(spans, "─", empty, palette.frame);
}

fn push_segment(
    spans: &mut Vec<Span<'static>>,
    glyph: &str,
    width: usize,
    color: ratatui::style::Color,
) {
    if width > 0 {
        spans.push(Span::styled(
            glyph.repeat(width),
            Style::default().fg(color),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_width::UnicodeWidthStr;

    fn display_width(line: &MeterLine) -> usize {
        line.spans
            .iter()
            .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
            .sum()
    }

    #[test]
    fn hides_when_the_input_header_is_too_narrow() {
        assert!(
            microphone_meter(
                MicrophoneState::Unmuted,
                Some(MicrophoneLevel { peak_db: -12.0 }),
                9,
                Palette::default(),
            )
            .is_none()
        );
    }

    #[test]
    fn keeps_compact_and_full_meter_widths_exact() {
        for available in [10, 17, 18, 24, 30, 48] {
            let meter = microphone_meter(
                MicrophoneState::Unmuted,
                Some(MicrophoneLevel { peak_db: -12.0 }),
                available,
                Palette::default(),
            )
            .unwrap();
            assert_eq!(meter.width, available.min(24));
            assert_eq!(display_width(&meter), meter.width);
        }
    }

    #[test]
    fn renders_a_static_muted_state() {
        let meter = microphone_meter(
            MicrophoneState::Muted,
            Some(MicrophoneLevel { peak_db: -3.0 }),
            20,
            Palette::default(),
        )
        .unwrap();
        let text = meter
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert_eq!(text, " MIC MUTE ");
    }
}
