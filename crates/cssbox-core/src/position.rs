//! CSS positioning: relative, absolute, fixed, sticky.
//!
//! Implements CSS 2.1 §9.3 and css-position-3.

use crate::fragment::Fragment;
use crate::geometry::{Point, Size};
use crate::style::{ComputedStyle, Position};
use crate::tree::BoxTree;

/// Resolve positioned elements after initial layout.
///
/// This walks the fragment tree and adjusts positions for:
/// - `position: relative` — offset from normal flow position
/// - `position: absolute` — positioned relative to the nearest positioned
///   ancestor's padding box (or the initial containing block)
/// - `position: fixed` — positioned relative to the viewport
///
/// Fragment positions are relative to the parent's content box, so the walk
/// tracks absolute origins to convert between the two.
pub fn resolve_positioned(tree: &BoxTree, mut root: Fragment, viewport: Size) -> Fragment {
    let cb = Cb {
        origin: Point::ZERO,
        size: viewport,
    };
    resolve_fragment(tree, &mut root, Point::ZERO, cb, viewport);
    root
}

/// A containing block's padding box in document coordinates.
#[derive(Clone, Copy)]
struct Cb {
    origin: Point,
    size: Size,
}

fn resolve_fragment(
    tree: &BoxTree,
    fragment: &mut Fragment,
    parent_origin: Point,
    cb: Cb,
    viewport: Size,
) {
    let style = tree.style(fragment.node);

    // Line boxes and anonymous boxes share their container's node (and so its
    // style); only real boxes are offset.
    let position = if fragment.kind == crate::fragment::FragmentKind::Box {
        style.position
    } else {
        Position::Static
    };
    match position {
        Position::Relative | Position::Sticky => {
            apply_relative_offset(fragment, style, cb.size);
        }
        Position::Absolute => {
            place_absolute(fragment, style, parent_origin, cb);
        }
        Position::Fixed => {
            let vcb = Cb {
                origin: Point::ZERO,
                size: viewport,
            };
            place_absolute(fragment, style, parent_origin, vcb);
        }
        Position::Static => {}
    }

    let border_box_origin = Point::new(
        parent_origin.x + fragment.position.x,
        parent_origin.y + fragment.position.y,
    );
    let content_origin = Point::new(
        border_box_origin.x + fragment.border.left + fragment.padding.left,
        border_box_origin.y + fragment.border.top + fragment.padding.top,
    );
    let child_cb = if position.is_positioned() {
        Cb {
            origin: Point::new(
                border_box_origin.x + fragment.border.left,
                border_box_origin.y + fragment.border.top,
            ),
            size: Size::new(
                fragment.size.width + fragment.padding.horizontal(),
                fragment.size.height + fragment.padding.vertical(),
            ),
        }
    } else {
        cb
    };

    for child in &mut fragment.children {
        resolve_fragment(tree, child, content_origin, child_cb, viewport);
    }
}

/// Apply relative positioning offsets.
/// CSS 2.1 §9.4.3: Relative positioning offsets the box from its normal flow position.
fn apply_relative_offset(fragment: &mut Fragment, style: &ComputedStyle, cb_size: Size) {
    let dx = resolve_offset_pair(&style.left, &style.right, cb_size.width);
    let dy = resolve_offset_pair(&style.top, &style.bottom, cb_size.height);

    fragment.position.x += dx;
    fragment.position.y += dy;
}

/// Place an absolutely positioned box. The fragment's current position is its
/// static position (relative to its parent's content box); axes with both
/// offsets `auto` keep it.
fn place_absolute(fragment: &mut Fragment, style: &ComputedStyle, parent_origin: Point, cb: Cb) {
    let bb_w = fragment.size.width + fragment.padding.horizontal() + fragment.border.horizontal();
    let mut bb_h =
        fragment.size.height + fragment.padding.vertical() + fragment.border.vertical();
    let left = style.left.resolve(cb.size.width);
    let right = style.right.resolve(cb.size.width);
    let top = style.top.resolve(cb.size.height);
    let bottom = style.bottom.resolve(cb.size.height);

    // Stretch the height between top and bottom when it is auto.
    if style.height.is_auto() {
        if let (Some(t), Some(b)) = (top, bottom) {
            let h = cb.size.height - t - b - fragment.margin.vertical();
            let extra = fragment.padding.vertical() + fragment.border.vertical();
            fragment.size.height = (h - extra).max(0.0);
            bb_h = fragment.size.height + extra;
        }
    }

    // Absolute document coordinates of the border box's top-left.
    let abs_x = match (left, right) {
        (Some(l), _) => Some(cb.origin.x + l + fragment.margin.left),
        (None, Some(r)) => {
            Some(cb.origin.x + cb.size.width - r - fragment.margin.right - bb_w)
        }
        _ => None,
    };
    let abs_y = match (top, bottom) {
        (Some(t), _) => Some(cb.origin.y + t + fragment.margin.top),
        (None, Some(b)) => {
            Some(cb.origin.y + cb.size.height - b - fragment.margin.bottom - bb_h)
        }
        _ => None,
    };
    if let Some(x) = abs_x {
        fragment.position.x = x - parent_origin.x;
    }
    if let Some(y) = abs_y {
        fragment.position.y = y - parent_origin.y;
    }
}

/// Resolve an offset pair (e.g., left/right or top/bottom) for relative positioning.
/// If both are specified, the start value wins (per CSS 2.1).
fn resolve_offset_pair(
    start: &crate::values::LengthPercentageAuto,
    end: &crate::values::LengthPercentageAuto,
    reference: f32,
) -> f32 {
    let s = start.resolve(reference);
    let e = end.resolve(reference);

    match (s, e) {
        (Some(sv), _) => sv,     // start wins
        (None, Some(ev)) => -ev, // end is negated
        (None, None) => 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::values::LengthPercentageAuto;

    #[test]
    fn test_relative_offset_left() {
        let left = LengthPercentageAuto::px(10.0);
        let right = LengthPercentageAuto::Auto;
        assert_eq!(resolve_offset_pair(&left, &right, 800.0), 10.0);
    }

    #[test]
    fn test_relative_offset_right() {
        let left = LengthPercentageAuto::Auto;
        let right = LengthPercentageAuto::px(10.0);
        assert_eq!(resolve_offset_pair(&left, &right, 800.0), -10.0);
    }

    #[test]
    fn test_relative_offset_both_start_wins() {
        let left = LengthPercentageAuto::px(20.0);
        let right = LengthPercentageAuto::px(10.0);
        assert_eq!(resolve_offset_pair(&left, &right, 800.0), 20.0);
    }
}
