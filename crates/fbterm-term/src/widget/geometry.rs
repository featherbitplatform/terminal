//! Pixel <-> grid conversions.

use alacritty_terminal::index::{Column, Line, Point};
use iced::Size;

use crate::GridSize;

/// Grid that fits `bounds` (logical px) with cells of `cell` size; at least 1x1.
pub fn grid_size_from_bounds(bounds: Size, cell: Size) -> GridSize {
    let fit = |px: f32, cell: f32| {
        if cell > 0.0 {
            (px / cell).floor().clamp(1.0, f32::from(u16::MAX)) as u16
        } else {
            1
        }
    };
    GridSize {
        cols: fit(bounds.width, cell.width),
        rows: fit(bounds.height, cell.height),
        cell_width: cell.width,
        cell_height: cell.height,
    }
}

/// Viewport cell `(col, row)` under `position` (relative to the widget origin), clamped to the grid.
pub fn pixel_to_cell(position: iced::Point, grid: GridSize) -> (u16, u16) {
    let clamp = |px: f32, cell: f32, count: u16| {
        let max = f32::from(count.max(1) - 1);
        if cell > 0.0 {
            (px / cell).floor().clamp(0.0, max) as u16
        } else {
            0
        }
    };
    (
        clamp(position.x, grid.cell_width, grid.cols),
        clamp(position.y, grid.cell_height, grid.rows),
    )
}

/// Grid point under `position` (relative to the widget origin), clamped to the visible grid and
/// shifted into history by `display_offset`.
pub fn pixel_to_point(position: iced::Point, grid: GridSize, display_offset: usize) -> Point {
    let (col, row) = pixel_to_cell(position, grid);
    Point::new(
        Line(i32::from(row) - display_offset as i32),
        Column(usize::from(col)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid(cols: u16, rows: u16) -> GridSize {
        GridSize {
            cols,
            rows,
            cell_width: 8.0,
            cell_height: 16.0,
        }
    }

    #[test]
    fn grid_size_from_bounds_floors_to_whole_cells() {
        let g = grid_size_from_bounds(Size::new(800.0, 600.0), Size::new(8.0, 16.0));
        assert_eq!((g.cols, g.rows), (100, 37));
        assert_eq!((g.cell_width, g.cell_height), (8.0, 16.0));
    }

    #[test]
    fn grid_size_from_bounds_is_at_least_one_cell() {
        let g = grid_size_from_bounds(Size::new(3.0, 0.0), Size::new(8.0, 16.0));
        assert_eq!((g.cols, g.rows), (1, 1));
    }

    #[test]
    fn pixel_to_point_maps_inside_cells() {
        let p = pixel_to_point(iced::Point::new(17.0, 33.0), grid(100, 37), 0);
        assert_eq!(p, Point::new(Line(2), Column(2)));
    }

    #[test]
    fn pixel_to_point_clamps_outside_bounds() {
        let g = grid(100, 37);
        assert_eq!(
            pixel_to_point(iced::Point::new(-5.0, -40.0), g, 0),
            Point::new(Line(0), Column(0))
        );
        assert_eq!(
            pixel_to_point(iced::Point::new(5000.0, 5000.0), g, 0),
            Point::new(Line(36), Column(99))
        );
    }

    #[test]
    fn pixel_to_point_accounts_for_scrollback_offset() {
        let p = pixel_to_point(iced::Point::new(0.0, 0.0), grid(100, 37), 5);
        assert_eq!(p, Point::new(Line(-5), Column(0)));
    }
}
