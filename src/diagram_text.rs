//! SVG outlines become terminal strokes; labels remain real Unicode text.
use crate::diagram::Geometry;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    symbols::Marker,
    widgets::{
        Paragraph,
        canvas::{Canvas, Line},
    },
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const CELL_WIDTH: f64 = 10.0;
const CELL_HEIGHT: f64 = 22.0;
pub fn natural_scale(geometry: &Geometry) -> f64 {
    geometry.labels.iter().fold(1.0_f64, |scale, label| {
        scale
            .max(label.text.width() as f64 * CELL_WIDTH / label.width.max(1.0))
            .max(CELL_HEIGHT / label.height.max(1.0))
    }) * 1.15
}

pub fn render(
    frame: &mut Frame,
    geometry: &Geometry,
    area: Rect,
    zoom: u16,
    pan: (&mut u32, &mut u32),
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    // Keep enough space for monospace labels even if the browser used a narrow font.
    let natural = natural_scale(geometry);
    let scale = natural * f64::from(zoom.max(100)) / 100.0;
    let width = f64::from(area.width) * CELL_WIDTH;
    let height = f64::from(area.height) * CELL_HEIGHT;
    *pan.0 = (*pan.0).min((geometry.width * scale - width).max(0.0).ceil() as u32);
    *pan.1 = (*pan.1).min((geometry.height * scale - height).max(0.0).ceil() as u32);
    let left = f64::from(*pan.0);
    let top = f64::from(*pan.1);
    let canvas = Canvas::default()
        .marker(Marker::Braille)
        .background_color(Color::Rgb(17, 23, 34))
        .x_bounds([left, left + width])
        .y_bounds([-(top + height), -top])
        .paint(|context| {
            for path in &geometry.paths {
                for segment in path.windows(2) {
                    context.draw(&Line {
                        x1: segment[0][0] * scale,
                        y1: -segment[0][1] * scale,
                        x2: segment[1][0] * scale,
                        y2: -segment[1][1] * scale,
                        color: Color::Rgb(76, 207, 176),
                    });
                }
            }
        });
    frame.render_widget(canvas, area);
    for label in &geometry.labels {
        let center = (label.x + label.width / 2.0) * scale;
        let x = ((center - left) / CELL_WIDTH - label.text.width() as f64 / 2.0).round() as i32;
        let y = (((label.y + label.height / 2.0) * scale - top) / CELL_HEIGHT).floor() as i32;
        if y < 0 || y >= i32::from(area.height) || x >= i32::from(area.width) {
            continue;
        }
        let (text, skipped) = clip_left(&label.text, x.saturating_neg().max(0) as usize);
        let x = (x + skipped as i32).max(0) as u16;
        if x >= area.width {
            continue;
        }
        frame.render_widget(
            Paragraph::new(text).style(
                Style::default()
                    .fg(Color::Rgb(212, 222, 239))
                    .bg(Color::Rgb(17, 23, 34)),
            ),
            Rect::new(
                area.x + x,
                area.y + y as u16,
                text.width().min(usize::from(area.width - x)) as u16,
                1,
            ),
        );
    }
}

fn clip_left(text: &str, columns: usize) -> (&str, usize) {
    let mut skipped = 0;
    let mut offset = 0;
    for (index, grapheme) in text.grapheme_indices(true) {
        if skipped >= columns {
            break;
        }
        skipped += grapheme.width();
        offset = index + grapheme.len();
    }
    (&text[offset..], skipped)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn horizontal_scroll_preserves_unicode_graphemes() {
        assert_eq!(clip_left("🦀 Привет", 1), (" Привет", 2));
        assert_eq!(clip_left("e\u{301}clair", 1), ("clair", 1));
        assert_eq!(clip_left("Rust", 99), ("", 4));
    }
}
