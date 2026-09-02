use crate::theme::Palette;
use anyhow::{Result, anyhow};
use qrcode::{Color as QrColor, EcLevel, QrCode};
use ratatui::{
    layout::{Alignment, Rect},
    style::Style,
    widgets::{Block, Borders, Clear, Paragraph},
};
use unicode_width::UnicodeWidthStr;

pub(super) fn draw_qr(frame: &mut ratatui::Frame, area: Rect, lines: &[String], palette: Palette) {
    let content_width = lines
        .iter()
        .map(|line| UnicodeWidthStr::width(line.as_str()) as u16)
        .max()
        .unwrap_or(20);
    let popup_width = content_width.saturating_add(2);
    let popup_height = u16::try_from(lines.len())
        .unwrap_or(u16::MAX)
        .saturating_add(2);
    let max_width = area.width.saturating_sub(2);
    let max_height = area.height.saturating_sub(2);

    if popup_width > max_width || popup_height > max_height {
        let popup = centered_rect(max_width.min(48), max_height.min(5), area);
        frame.render_widget(Clear, popup);
        frame.render_widget(
            Paragraph::new("终端空间不足\n调整窗口后二维码会自动显示")
                .alignment(Alignment::Center)
                .style(Style::default().fg(palette.warning).bg(palette.background))
                .block(
                    Block::default()
                        .title("B 站扫码登录 · Esc 关闭")
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(palette.info)),
                ),
            popup,
        );
        return;
    }

    let popup = centered_rect(popup_width, popup_height, area);
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines.join("\n"))
            .alignment(Alignment::Center)
            .style(Style::default().fg(palette.content).bg(palette.background))
            .block(
                Block::default()
                    .title("B 站扫码登录 · Esc 关闭")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(palette.info)),
            ),
        popup,
    );
}

pub fn qr_lines(value: &str) -> Result<Vec<String>> {
    qr_lines_with_level(value, EcLevel::M)
}

pub(super) fn compact_qr_lines(value: &str) -> Result<Vec<String>> {
    qr_lines_with_level(value, EcLevel::L)
}

fn qr_lines_with_level(value: &str, level: EcLevel) -> Result<Vec<String>> {
    let code = QrCode::with_error_correction_level(value.as_bytes(), level)
        .map_err(|error| anyhow!(error.to_string()))?;
    let width = code.width();
    let colors = code.to_colors();
    let quiet = 2usize;
    let full = width + quiet * 2;
    let mut lines = Vec::new();
    for y in (0..full).step_by(2) {
        let mut line = String::new();
        for x in 0..full {
            let top = if x < quiet || x >= width + quiet || y < quiet || y >= width + quiet {
                false
            } else {
                colors[(y - quiet) * width + (x - quiet)] == QrColor::Dark
            };
            let bottom_y = y + 1;
            let bottom =
                if x < quiet || x >= width + quiet || bottom_y < quiet || bottom_y >= width + quiet
                {
                    false
                } else {
                    colors[(bottom_y - quiet) * width + (x - quiet)] == QrColor::Dark
                };
            line.push(match (top, bottom) {
                (true, true) => '█',
                (true, false) => '▀',
                (false, true) => '▄',
                (false, false) => ' ',
            });
        }
        lines.push(line);
    }
    Ok(lines)
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}
