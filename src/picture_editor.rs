//! The geometry behind the picture editor: a crop rectangle over a possibly rotated picture, with
//! aspect-ratio locking, handle dragging and keyboard nudging. It is pure arithmetic, so it is
//! tested exhaustively without a display, and every operation keeps the crop inside the picture.
//!
//! Coordinates: the crop is kept in *view* pixels, the picture as the user currently sees it
//! (after rotation). `job()` converts it to the source-pixel crop plus rotation that the sandboxed
//! helper applies, which crops first and rotates second (see `media_authoring`).
use crate::media_authoring::{Job, Mark, Rect, Rotation};

/// Smallest crop edge in pixels (or the picture edge when the picture is smaller).
pub const MIN_EDGE: u32 = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Aspect {
    Free,
    Square,
    /// The ratio of the whole picture as currently shown.
    Original,
    Ratio(u32, u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handle {
    Move,
    TopLeft,
    Top,
    TopRight,
    Right,
    BottomRight,
    Bottom,
    BottomLeft,
    Left,
}

/// How thick a mark's outline is, relative to the picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Weight {
    Thin,
    Medium,
    Thick,
}

/// The colours a mark can be, by name.
pub const INKS: [(&str, [u8; 3]); 6] = [
    ("Red", [230, 40, 40]),
    ("Yellow", [250, 200, 30]),
    ("Green", [50, 180, 80]),
    ("Blue", [50, 110, 230]),
    ("White", [255, 255, 255]),
    ("Black", [0, 0, 0]),
];

/// Line thicknesses by name, in the order the picker lists them.
pub const WEIGHTS: [(&str, Weight); 3] = [("Thin", Weight::Thin), ("Medium", Weight::Medium), ("Thick", Weight::Thick)];

/// Most marks one picture may carry (the helper refuses more).
pub const MAX_MARKS: usize = 32;
/// Smallest mark, in view pixels. A click that barely moved is not a shape.
pub const MIN_MARK: u32 = 8;

/// A square the user drew, in view pixels, so it turns with the picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ViewMark {
    pub rect: Rect,
    pub color: [u8; 3],
    pub stroke: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Editor {
    source: (u32, u32),
    rotation: Rotation,
    /// In view pixels.
    rect: Rect,
    aspect: Aspect,
    marks: Vec<ViewMark>,
}

fn turn(rotation: Rotation, clockwise: bool) -> Rotation {
    use Rotation::*;
    match (rotation, clockwise) {
        (None, true) | (Half, false) => Clockwise90,
        (Clockwise90, true) | (Counterclockwise90, false) => Half,
        (Half, true) | (None, false) => Counterclockwise90,
        (Counterclockwise90, true) | (Clockwise90, false) => None,
    }
}

/// Carry a rectangle along with the picture when the picture turns a quarter, `(width, height)`
/// being the size of the picture *before* the turn.
fn turn_rect(rect: Rect, (width, height): (u32, u32), clockwise: bool) -> Rect {
    if clockwise {
        Rect { x: height - rect.y - rect.height, y: rect.x, width: rect.height, height: rect.width }
    } else {
        Rect { x: rect.y, y: width - rect.x - rect.width, width: rect.height, height: rect.width }
    }
}

impl Editor {
    /// An editor over a picture of `source` pixels (as the helper reports it, orientation applied),
    /// starting with the whole picture selected.
    pub fn new(width: u32, height: u32) -> Self {
        let (width, height) = (width.max(1), height.max(1));
        Self {
            source: (width, height),
            rotation: Rotation::None,
            rect: Rect { x: 0, y: 0, width, height },
            aspect: Aspect::Free,
            marks: Vec::new(),
        }
    }
    pub fn rotation(&self) -> Rotation {
        self.rotation
    }
    pub fn aspect(&self) -> Aspect {
        self.aspect
    }
    pub fn rect(&self) -> Rect {
        self.rect
    }
    /// The picture as shown: the source with the rotation applied.
    pub fn view_size(&self) -> (u32, u32) {
        match self.rotation {
            Rotation::Clockwise90 | Rotation::Counterclockwise90 => (self.source.1, self.source.0),
            _ => self.source,
        }
    }
    pub fn is_unchanged(&self) -> bool {
        self.marks.is_empty() && self.rotation == Rotation::None && self.rect == Rect { x: 0, y: 0, width: self.source.0, height: self.source.1 }
    }
    pub fn reset(&mut self) {
        *self = Self::new(self.source.0, self.source.1);
    }

    pub fn marks(&self) -> &[ViewMark] {
        &self.marks
    }

    /// Outline thickness in pixels: a share of the picture's longest edge, never under 2.
    pub fn stroke_for(&self, weight: Weight) -> u32 {
        let longest = self.source.0.max(self.source.1);
        let per_mille = match weight {
            Weight::Thin => 3,
            Weight::Medium => 6,
            Weight::Thick => 12,
        };
        (longest * per_mille / 1000).clamp(2, 128)
    }

    /// Add a square over `rect` (view pixels, clamped into the picture). Refused, returning false,
    /// when it is too small or the picture already has `MAX_MARKS`.
    pub fn add_square(&mut self, rect: Rect, color: [u8; 3], weight: Weight) -> bool {
        let (view_w, view_h) = self.view_size();
        let x = rect.x.min(view_w.saturating_sub(1));
        let y = rect.y.min(view_h.saturating_sub(1));
        let rect = Rect { x, y, width: rect.width.min(view_w - x), height: rect.height.min(view_h - y) };
        if self.marks.len() >= MAX_MARKS || rect.width < MIN_MARK.min(view_w) || rect.height < MIN_MARK.min(view_h) {
            return false;
        }
        let stroke = self.stroke_for(weight);
        self.marks.push(ViewMark { rect, color, stroke });
        true
    }

    /// Outline the crop area as it is now: the way to place a square without a pointer.
    pub fn mark_crop(&mut self, color: [u8; 3], weight: Weight) -> bool {
        self.add_square(self.rect, color, weight)
    }

    /// Take back the newest mark.
    pub fn undo_mark(&mut self) -> bool {
        self.marks.pop().is_some()
    }

    /// Turn the picture a quarter; the crop and the marks turn with it.
    pub fn rotate(&mut self, clockwise: bool) {
        let before = self.view_size();
        for mark in &mut self.marks {
            mark.rect = turn_rect(mark.rect, before, clockwise);
        }
        self.rect = turn_rect(self.rect, before, clockwise);
        self.rotation = turn(self.rotation, clockwise);
        // Re-apply the lock: a locked 4:3 crop becomes 3:4 only if the user asks for it.
        let aspect = self.aspect;
        self.set_aspect(aspect);
    }

    fn min_size(&self) -> (u32, u32) {
        let (width, height) = self.view_size();
        (MIN_EDGE.min(width), MIN_EDGE.min(height))
    }

    /// `width / height` the crop must keep, if locked.
    fn ratio(&self) -> Option<(u32, u32)> {
        match self.aspect {
            Aspect::Free => None,
            Aspect::Square => Some((1, 1)),
            Aspect::Original => Some(self.view_size()),
            Aspect::Ratio(w, h) if w > 0 && h > 0 => Some((w, h)),
            Aspect::Ratio(..) => None,
        }
    }

    /// Lock the crop to `aspect`, shrinking it around its centre so it still fits.
    pub fn set_aspect(&mut self, aspect: Aspect) {
        self.aspect = aspect;
        let Some((rw, rh)) = self.ratio() else { return };
        let (view_w, view_h) = self.view_size();
        let (cx2, cy2) = (2 * i64::from(self.rect.x) + i64::from(self.rect.width), 2 * i64::from(self.rect.y) + i64::from(self.rect.height));
        // Largest rectangle of the ratio inside the current one.
        let (mut width, mut height) = (i64::from(self.rect.width), i64::from(self.rect.height));
        if width * i64::from(rh) > height * i64::from(rw) {
            width = (height * i64::from(rw) + i64::from(rh) / 2) / i64::from(rh);
        } else {
            height = (width * i64::from(rh) + i64::from(rw) / 2) / i64::from(rw);
        }
        self.rect = place(cx2, cy2, width.max(1), height.max(1), view_w, view_h);
    }

    /// Slide the whole crop, staying inside the picture.
    pub fn move_by(&mut self, dx: i64, dy: i64) {
        let (view_w, view_h) = self.view_size();
        let clamp = |value: i64, size: u32, limit: u32| value.clamp(0, i64::from(limit - size));
        self.rect.x = clamp(i64::from(self.rect.x) + dx, self.rect.width, view_w) as u32;
        self.rect.y = clamp(i64::from(self.rect.y) + dy, self.rect.height, view_h) as u32;
    }

    /// Drag `handle` by `(dx, dy)` view pixels from where it started (`start`, the crop at the
    /// beginning of the drag). The opposite edge or corner stays put; with a locked aspect the
    /// ratio is kept; the result is always inside the picture and at least the minimum size.
    pub fn drag(&mut self, handle: Handle, start: Rect, dx: i64, dy: i64) {
        if handle == Handle::Move {
            self.rect = start;
            self.move_by(dx, dy);
            return;
        }
        let (view_w, view_h) = self.view_size();
        let (view_w, view_h) = (i64::from(view_w), i64::from(view_h));
        let (min_w, min_h) = self.min_size();
        let (min_w, min_h) = (i64::from(min_w), i64::from(min_h));
        let (left, top) = (i64::from(start.x), i64::from(start.y));
        let (right, bottom) = (left + i64::from(start.width), top + i64::from(start.height));
        let (moves_left, moves_right, moves_top, moves_bottom) = match handle {
            Handle::TopLeft => (true, false, true, false),
            Handle::Top => (false, false, true, false),
            Handle::TopRight => (false, true, true, false),
            Handle::Right => (false, true, false, false),
            Handle::BottomRight => (false, true, false, true),
            Handle::Bottom => (false, false, false, true),
            Handle::BottomLeft => (true, false, false, true),
            Handle::Left => (true, false, false, false),
            Handle::Move => unreachable!(),
        };
        // Where the dragged edges want to be, kept in the picture and at least the minimum size
        // away from the fixed ones.
        let new_left = if moves_left { (left + dx).clamp(0, right - min_w) } else { left };
        let new_right = if moves_right { (right + dx).clamp(left + min_w, view_w) } else { right };
        let new_top = if moves_top { (top + dy).clamp(0, bottom - min_h) } else { top };
        let new_bottom = if moves_bottom { (bottom + dy).clamp(top + min_h, view_h) } else { bottom };
        let (mut width, mut height) = (new_right - new_left, new_bottom - new_top);

        if let Some((rw, rh)) = self.ratio().map(|(w, h)| (i64::from(w), i64::from(h))) {
            let horizontal = moves_left || moves_right;
            let vertical = moves_top || moves_bottom;
            // Space available from the fixed side, and (for edge handles) around the centre.
            let room_w = if moves_left { right } else if moves_right { view_w - left } else { view_w };
            let room_h = if moves_top { bottom } else if moves_bottom { view_h - top } else { view_h };
            let (centre_x2, centre_y2) = (left + right, top + bottom);
            let around_w = centre_x2.min(2 * view_w - centre_x2);
            let around_h = centre_y2.min(2 * view_h - centre_y2);
            let (max_w, max_h) = (
                if horizontal && vertical { room_w } else if horizontal { room_w } else { around_w },
                if horizontal && vertical { room_h } else if vertical { room_h } else { around_h },
            );
            // The drag that moved furthest sets the size; the other side follows the ratio.
            width = if horizontal && vertical {
                width.max((height * rw + rh / 2) / rh)
            } else if horizontal {
                width
            } else {
                (height * rw + rh / 2) / rh
            };
            width = width.min(max_w).min((max_h * rw) / rh).max(min_w.max((min_h * rw + rh - 1) / rh)).min(view_w);
            height = ((width * rh + rw / 2) / rw).clamp(1, max_h.max(1)).min(view_h);
            // Rounding the height may have shrunk past the ratio; fix the width to match.
            width = ((height * rw + rh / 2) / rh).clamp(1, view_w);
            let (x, y) = anchor(handle, left, top, right, bottom, width, height);
            self.rect = fit(x, y, width, height, view_w, view_h);
            return;
        }
        self.rect = fit(new_left, new_top, width, height, view_w, view_h);
    }

    /// Set the crop from typed numbers (view pixels), clamping to the picture and the minimum.
    pub fn set_rect(&mut self, x: i64, y: i64, width: i64, height: i64) {
        let (view_w, view_h) = self.view_size();
        let (view_w, view_h) = (i64::from(view_w), i64::from(view_h));
        let (min_w, min_h) = self.min_size();
        let width = width.clamp(i64::from(min_w), view_w);
        let height = height.clamp(i64::from(min_h), view_h);
        self.rect = Rect {
            x: x.clamp(0, view_w - width) as u32,
            y: y.clamp(0, view_h - height) as u32,
            width: width as u32,
            height: height as u32,
        };
        // Typed numbers win over the lock rather than fighting the user.
        self.aspect = Aspect::Free;
    }

    /// A rectangle in view pixels as source pixels: the view is the source turned, so turn it back.
    fn to_source(&self, rect: Rect) -> Rect {
        let view = self.view_size();
        match self.rotation {
            Rotation::None => rect,
            Rotation::Clockwise90 => turn_rect(rect, view, false),
            Rotation::Counterclockwise90 => turn_rect(rect, view, true),
            Rotation::Half => turn_rect(turn_rect(rect, view, true), (view.1, view.0), true),
        }
    }

    /// The crop, rotation and marks as the sandboxed helper wants them: source pixels, marks
    /// drawn first, then crop, then rotation.
    pub fn job(&self, max_edge: Option<u32>) -> Job {
        let crop = self.to_source(self.rect);
        let whole = Rect { x: 0, y: 0, width: self.source.0, height: self.source.1 };
        let marks = self
            .marks
            .iter()
            .map(|mark| Mark::Square { rect: self.to_source(mark.rect), color: mark.color, stroke: mark.stroke })
            .collect();
        Job { crop: (crop != whole).then_some(crop), rotation: self.rotation, trim: None, max_edge, marks }
    }
}

/// A rectangle of the given size centred on `(centre_x2 / 2, centre_y2 / 2)`, inside the picture.
fn place(centre_x2: i64, centre_y2: i64, width: i64, height: i64, view_w: u32, view_h: u32) -> Rect {
    let (view_w, view_h) = (i64::from(view_w), i64::from(view_h));
    let (width, height) = (width.clamp(1, view_w), height.clamp(1, view_h));
    Rect {
        x: ((centre_x2 - width) / 2).clamp(0, view_w - width) as u32,
        y: ((centre_y2 - height) / 2).clamp(0, view_h - height) as u32,
        width: width as u32,
        height: height as u32,
    }
}

/// Where the top-left of a resized rectangle goes so the opposite edge or corner stays fixed.
fn anchor(handle: Handle, left: i64, top: i64, right: i64, bottom: i64, width: i64, height: i64) -> (i64, i64) {
    let x = match handle {
        Handle::TopLeft | Handle::BottomLeft | Handle::Left => right - width,
        Handle::TopRight | Handle::BottomRight | Handle::Right => left,
        _ => (left + right - width) / 2,
    };
    let y = match handle {
        Handle::TopLeft | Handle::TopRight | Handle::Top => bottom - height,
        Handle::BottomLeft | Handle::BottomRight | Handle::Bottom => top,
        _ => (top + bottom - height) / 2,
    };
    (x, y)
}

fn fit(x: i64, y: i64, width: i64, height: i64, view_w: i64, view_h: i64) -> Rect {
    let (width, height) = (width.clamp(1, view_w), height.clamp(1, view_h));
    Rect {
        x: x.clamp(0, view_w - width) as u32,
        y: y.clamp(0, view_h - height) as u32,
        width: width as u32,
        height: height as u32,
    }
}

/// Which part of the crop is under a point, for choosing the cursor and the drag to start. All
/// values are widget pixels; `grab` is the size of a handle's hit area.
pub fn hit(point: (f64, f64), crop: (f64, f64, f64, f64), grab: f64) -> Option<Handle> {
    let (x, y, width, height) = crop;
    let (right, bottom) = (x + width, y + height);
    let near = |value: f64, edge: f64| (value - edge).abs() <= grab;
    let (inside_x, inside_y) = (point.0 >= x - grab && point.0 <= right + grab, point.1 >= y - grab && point.1 <= bottom + grab);
    if !inside_x || !inside_y {
        return None;
    }
    let (left_edge, right_edge, top_edge, bottom_edge) = (near(point.0, x), near(point.0, right), near(point.1, y), near(point.1, bottom));
    // Corners win over edges; on a very small crop the nearer side wins.
    Some(match (left_edge, right_edge, top_edge, bottom_edge) {
        (true, false, true, false) => Handle::TopLeft,
        (false, true, true, false) => Handle::TopRight,
        (true, false, false, true) => Handle::BottomLeft,
        (false, true, false, true) => Handle::BottomRight,
        (true, false, _, _) => Handle::Left,
        (false, true, _, _) => Handle::Right,
        (_, _, true, false) => Handle::Top,
        (_, _, false, true) => Handle::Bottom,
        _ if point.0 >= x && point.0 <= right && point.1 >= y && point.1 <= bottom => Handle::Move,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inside(editor: &Editor) -> bool {
        let (w, h) = editor.view_size();
        let r = editor.rect();
        r.width >= 1 && r.height >= 1 && u64::from(r.x) + u64::from(r.width) <= u64::from(w) && u64::from(r.y) + u64::from(r.height) <= u64::from(h)
    }
    const HANDLES: [Handle; 9] = [
        Handle::Move, Handle::TopLeft, Handle::Top, Handle::TopRight, Handle::Right,
        Handle::BottomRight, Handle::Bottom, Handle::BottomLeft, Handle::Left,
    ];

    #[test]
    fn a_new_editor_selects_everything_and_is_unchanged() {
        let editor = Editor::new(640, 480);
        assert_eq!(editor.rect(), Rect { x: 0, y: 0, width: 640, height: 480 });
        assert!(editor.is_unchanged());
        assert_eq!(editor.job(None), Job::default(), "no edit means no crop and no rotation");
    }

    #[test]
    fn four_turns_return_to_the_start_and_the_crop_turns_with_the_picture() {
        for clockwise in [true, false] {
            let mut editor = Editor::new(400, 300);
            editor.set_rect(40, 30, 200, 100);
            let start = editor.rect();
            for _ in 0..4 {
                editor.rotate(clockwise);
                assert!(inside(&editor));
            }
            assert_eq!(editor.rect(), start);
            assert_eq!(editor.rotation(), Rotation::None);
        }
        // One clockwise turn of a 400x300 picture is 300x400; a crop in its top-left corner
        // ends up in the top-right.
        let mut editor = Editor::new(400, 300);
        editor.set_rect(0, 0, 100, 50);
        editor.rotate(true);
        assert_eq!(editor.view_size(), (300, 400));
        assert_eq!(editor.rect(), Rect { x: 250, y: 0, width: 50, height: 100 });
        // Turning back counterclockwise is the inverse.
        editor.rotate(false);
        assert_eq!(editor.rect(), Rect { x: 0, y: 0, width: 100, height: 50 });
    }

    /// Map a view point back to the source pixel it shows, the long way round, to check `job`.
    fn source_point(rotation: Rotation, source: (u32, u32), view: (u32, u32)) -> impl Fn((u32, u32)) -> (u32, u32) {
        move |(x, y)| match rotation {
            Rotation::None => (x, y),
            Rotation::Clockwise90 => (y, source.1 - 1 - x),
            Rotation::Half => (source.0 - 1 - x, source.1 - 1 - y),
            Rotation::Counterclockwise90 => (source.0 - 1 - y, x),
        }
        .pipe(move |p| { let _ = view; p })
    }
    trait Pipe: Sized { fn pipe<T>(self, f: impl FnOnce(Self) -> T) -> T { f(self) } }
    impl<T> Pipe for T {}

    #[test]
    fn the_job_crop_covers_exactly_the_source_pixels_the_user_sees() {
        for turns in 0..4 {
            let mut editor = Editor::new(400, 300);
            for _ in 0..turns {
                editor.rotate(true);
            }
            let view = editor.view_size();
            editor.set_rect(i64::from(view.0) / 8, i64::from(view.1) / 5, i64::from(view.0) / 2, i64::from(view.1) / 3);
            let (rect, job) = (editor.rect(), editor.job(None));
            let crop = job.crop.expect("a smaller crop is sent");
            let map = source_point(editor.rotation(), (400, 300), view);
            // Every corner of the visible rectangle lies in the source crop, and the source crop
            // is no bigger than the rectangle (so nothing outside it is included).
            for corner in [(rect.x, rect.y), (rect.x + rect.width - 1, rect.y), (rect.x, rect.y + rect.height - 1), (rect.x + rect.width - 1, rect.y + rect.height - 1)] {
                let (sx, sy) = map(corner);
                assert!(sx >= crop.x && sx < crop.x + crop.width && sy >= crop.y && sy < crop.y + crop.height, "turns {turns}: {corner:?} -> {:?} not in {crop:?}", (sx, sy));
            }
            assert_eq!(u64::from(crop.width) * u64::from(crop.height), u64::from(rect.width) * u64::from(rect.height), "turns {turns}");
            assert!(crop.x + crop.width <= 400 && crop.y + crop.height <= 300);
            assert_eq!(job.rotation, editor.rotation());
        }
    }

    #[test]
    fn locking_an_aspect_shrinks_the_crop_around_its_centre_inside_the_picture() {
        let mut editor = Editor::new(1000, 800);
        editor.set_aspect(Aspect::Square);
        assert_eq!(editor.rect(), Rect { x: 100, y: 0, width: 800, height: 800 });
        editor.set_aspect(Aspect::Ratio(16, 9));
        let r = editor.rect();
        assert_eq!(r.width, 800);
        assert!((i64::from(r.width) * 9 - i64::from(r.height) * 16).abs() <= 9, "{r:?}");
        editor.set_aspect(Aspect::Free);
        assert_eq!(editor.rect(), r, "unlocking does not move the crop");
        // Original keeps the ratio of the whole picture as shown, including after a turn.
        let mut editor = Editor::new(400, 200);
        editor.rotate(true);
        editor.set_aspect(Aspect::Original);
        assert_eq!(editor.view_size(), (200, 400));
        assert_eq!(editor.rect(), Rect { x: 0, y: 0, width: 200, height: 400 });
    }

    #[test]
    fn moving_slides_the_crop_but_never_out_of_the_picture() {
        let mut editor = Editor::new(500, 400);
        editor.set_rect(100, 100, 200, 100);
        editor.move_by(50, -30);
        assert_eq!((editor.rect().x, editor.rect().y), (150, 70));
        for (dx, dy) in [(1_000_000, 0), (-1_000_000, 0), (0, 1_000_000), (0, -1_000_000), (i64::MAX, i64::MIN)] {
            editor.move_by(dx, dy);
            assert!(inside(&editor), "{dx},{dy}: {:?}", editor.rect());
            assert_eq!((editor.rect().width, editor.rect().height), (200, 100), "size is untouched");
        }
    }

    #[test]
    fn dragging_a_handle_keeps_the_opposite_side_fixed() {
        let start = Rect { x: 100, y: 100, width: 200, height: 100 };
        let mut editor = Editor::new(500, 400);
        editor.set_rect(100, 100, 200, 100);
        editor.drag(Handle::Right, start, 50, 999);
        assert_eq!(editor.rect(), Rect { x: 100, y: 100, width: 250, height: 100 }, "an edge handle moves one edge");
        editor.drag(Handle::TopLeft, start, -40, -30);
        assert_eq!(editor.rect(), Rect { x: 60, y: 70, width: 240, height: 130 });
        editor.drag(Handle::Bottom, start, 0, 25);
        assert_eq!(editor.rect(), Rect { x: 100, y: 100, width: 200, height: 125 });
        editor.drag(Handle::Move, start, 10, 20);
        assert_eq!(editor.rect(), Rect { x: 110, y: 120, width: 200, height: 100 });
    }

    #[test]
    fn dragging_past_the_other_side_stops_at_the_minimum_size_instead_of_flipping() {
        let start = Rect { x: 100, y: 100, width: 200, height: 100 };
        let mut editor = Editor::new(500, 400);
        editor.set_rect(100, 100, 200, 100);
        editor.drag(Handle::Right, start, -10_000, 0);
        assert_eq!(editor.rect(), Rect { x: 100, y: 100, width: MIN_EDGE, height: 100 });
        editor.drag(Handle::TopLeft, start, 10_000, 10_000);
        let r = editor.rect();
        assert_eq!((r.x + r.width, r.y + r.height), (300, 200), "the bottom-right corner stayed");
        assert_eq!((r.width, r.height), (MIN_EDGE, MIN_EDGE));
    }

    #[test]
    fn a_locked_aspect_is_kept_by_every_handle_and_stays_in_bounds() {
        for ratio in [Aspect::Square, Aspect::Ratio(4, 3), Aspect::Ratio(16, 9), Aspect::Ratio(3, 4), Aspect::Original] {
            for rotations in [0, 1] {
                let mut editor = Editor::new(640, 480);
                for _ in 0..rotations {
                    editor.rotate(true);
                }
                editor.set_aspect(ratio);
                let (rw, rh) = editor.ratio().unwrap();
                let start = editor.rect();
                for handle in HANDLES.iter().copied().filter(|h| *h != Handle::Move) {
                    for (dx, dy) in [(20, 10), (-30, -50), (400, 300), (-400, -300), (0, 90), (90, 0), (i64::MAX / 4, i64::MIN / 4)] {
                        editor.drag(handle, start, dx, dy);
                        let r = editor.rect();
                        assert!(inside(&editor), "{ratio:?} {handle:?} {dx},{dy}: {r:?}");
                        // Within a pixel of the ratio (integer rounding), unless pinned by the minimum.
                        let off = (i64::from(r.width) * i64::from(rh) - i64::from(r.height) * i64::from(rw)).abs();
                        assert!(off <= i64::from(rw.max(rh)), "{ratio:?} {handle:?} {dx},{dy}: {r:?} off by {off}");
                    }
                }
            }
        }
    }

    #[test]
    fn an_edge_handle_with_a_locked_aspect_resizes_around_the_centre_of_the_other_axis() {
        let mut editor = Editor::new(800, 600);
        editor.set_rect(200, 150, 400, 300);
        editor.set_aspect(Aspect::Ratio(4, 3));
        let start = editor.rect();
        editor.drag(Handle::Right, start, -100, 0);
        let r = editor.rect();
        assert_eq!((r.x, r.width, r.height), (200, 300, 225), "{r:?}");
        // Whole pixels: an odd height cannot be centred exactly, so allow one pixel.
        let (now, before) = (i64::from(r.y) * 2 + i64::from(r.height), i64::from(start.y) * 2 + i64::from(start.height));
        assert!((now - before).abs() <= 1, "centred vertically: {now} vs {before}");
    }

    #[test]
    fn typed_numbers_are_clamped_and_release_the_lock() {
        let mut editor = Editor::new(300, 200);
        editor.set_aspect(Aspect::Square);
        editor.set_rect(-50, -50, 10_000, 10_000);
        assert_eq!(editor.rect(), Rect { x: 0, y: 0, width: 300, height: 200 });
        assert_eq!(editor.aspect(), Aspect::Free);
        editor.set_rect(290, 190, 0, 0);
        assert!(inside(&editor));
        assert_eq!((editor.rect().width, editor.rect().height), (MIN_EDGE, MIN_EDGE));
        assert_eq!((editor.rect().x, editor.rect().y), (284, 184), "kept inside after the minimum was applied");
    }

    #[test]
    fn pictures_smaller_than_the_minimum_still_work() {
        for (w, h) in [(1, 1), (5, 3), (16, 2)] {
            let mut editor = Editor::new(w, h);
            for handle in HANDLES {
                editor.drag(handle, editor.rect(), -999, 999);
                editor.drag(handle, editor.rect(), 999, -999);
                assert!(inside(&editor), "{w}x{h} {handle:?}");
            }
            editor.set_aspect(Aspect::Square);
            editor.rotate(true);
            assert!(inside(&editor));
            assert!(editor.job(None).crop.is_none_or(|c| c.width >= 1 && c.height >= 1));
        }
    }

    #[test]
    fn reset_undoes_every_change() {
        let mut editor = Editor::new(640, 480);
        editor.set_rect(10, 10, 100, 100);
        editor.rotate(false);
        editor.set_aspect(Aspect::Ratio(4, 3));
        editor.reset();
        assert!(editor.is_unchanged());
        assert_eq!((editor.aspect(), editor.rotation()), (Aspect::Free, Rotation::None));
    }

    #[test]
    fn a_rotation_alone_is_an_edit_with_no_crop() {
        let mut editor = Editor::new(640, 480);
        editor.rotate(true);
        let job = editor.job(Some(1280));
        assert_eq!((job.crop, job.rotation, job.max_edge), (None, Rotation::Clockwise90, Some(1280)));
        assert!(!editor.is_unchanged());
    }

    #[test]
    fn hit_testing_prefers_corners_then_edges_then_the_inside() {
        let crop = (100., 100., 200., 100.);
        let at = |x, y| hit((x, y), crop, 8.);
        assert_eq!(at(100., 100.), Some(Handle::TopLeft));
        assert_eq!(at(302., 198.), Some(Handle::BottomRight));
        assert_eq!(at(300., 150.), Some(Handle::Right));
        assert_eq!(at(200., 96.), Some(Handle::Top));
        assert_eq!(at(200., 150.), Some(Handle::Move));
        assert_eq!(at(50., 150.), None, "well outside");
        assert_eq!(at(95., 150.), Some(Handle::Left), "the handle area reaches a little outside");
        // On a crop smaller than two handles, neither corner swallows the whole thing.
        assert!(hit((105., 105.), (100., 100., 10., 10.), 8.).is_some());
    }

    const RED: [u8; 3] = [255, 0, 0];

    #[test]
    fn a_square_is_clamped_into_the_picture_and_tiny_ones_are_refused() {
        let mut editor = Editor::new(200, 100);
        assert!(editor.add_square(Rect { x: 150, y: 80, width: 500, height: 500 }, RED, Weight::Medium));
        assert_eq!(editor.marks()[0].rect, Rect { x: 150, y: 80, width: 50, height: 20 });
        assert!(!editor.add_square(Rect { x: 10, y: 10, width: MIN_MARK - 1, height: 50 }, RED, Weight::Thin));
        assert!(!editor.add_square(Rect { x: 10, y: 10, width: 50, height: 0 }, RED, Weight::Thin));
        assert_eq!(editor.marks().len(), 1);
        // A square starting past the edge is pulled back to the last pixel and then refused as tiny.
        assert!(!editor.add_square(Rect { x: 9999, y: 9999, width: 40, height: 40 }, RED, Weight::Thin));
    }

    #[test]
    fn at_most_the_helpers_limit_of_marks_can_be_added() {
        let mut editor = Editor::new(200, 100);
        for _ in 0..MAX_MARKS {
            assert!(editor.add_square(Rect { x: 0, y: 0, width: 20, height: 20 }, RED, Weight::Thin));
        }
        assert!(!editor.add_square(Rect { x: 0, y: 0, width: 20, height: 20 }, RED, Weight::Thin));
        assert_eq!(editor.marks().len(), MAX_MARKS);
        assert_eq!(MAX_MARKS, crate::media_authoring::Limits::default().max_marks);
    }

    #[test]
    fn line_thickness_follows_the_picture_size() {
        let small = Editor::new(100, 80);
        let large = Editor::new(4000, 3000);
        assert_eq!(small.stroke_for(Weight::Thin), 2, "never thinner than two pixels");
        assert!(large.stroke_for(Weight::Thin) < large.stroke_for(Weight::Medium));
        assert!(large.stroke_for(Weight::Medium) < large.stroke_for(Weight::Thick));
        assert_eq!(large.stroke_for(Weight::Medium), 24);
        let huge = Editor::new(16384, 16384);
        assert!(huge.stroke_for(Weight::Thick) <= crate::media_authoring::Limits::default().max_stroke);
    }

    #[test]
    fn marks_count_as_a_change_and_undo_and_reset_remove_them() {
        let mut editor = Editor::new(200, 100);
        assert!(editor.is_unchanged());
        assert!(!editor.undo_mark(), "nothing to undo");
        editor.add_square(Rect { x: 10, y: 10, width: 30, height: 30 }, RED, Weight::Thin);
        editor.add_square(Rect { x: 50, y: 10, width: 30, height: 30 }, [0, 0, 255], Weight::Thin);
        assert!(!editor.is_unchanged());
        assert!(editor.undo_mark());
        assert_eq!(editor.marks().len(), 1);
        assert_eq!(editor.marks()[0].color, RED, "the newest mark goes first");
        editor.reset();
        assert!(editor.marks().is_empty() && editor.is_unchanged());
    }

    #[test]
    fn a_square_can_be_placed_over_the_crop_without_a_pointer() {
        let mut editor = Editor::new(200, 100);
        editor.set_rect(20, 10, 100, 60);
        assert!(editor.mark_crop(RED, Weight::Medium));
        assert_eq!(editor.marks()[0].rect, editor.rect());
        editor.set_rect(0, 0, 50, 50);
        assert_eq!(editor.marks()[0].rect, Rect { x: 20, y: 10, width: 100, height: 60 }, "the mark stays put");
    }

    #[test]
    fn marks_reach_the_helper_in_source_pixels_for_every_rotation() {
        let mark = Rect { x: 10, y: 20, width: 30, height: 40 };
        let mut editor = Editor::new(200, 100);
        editor.add_square(mark, RED, Weight::Thin);
        let source = |editor: &Editor| match editor.job(None).marks[0] {
            Mark::Square { rect, .. } => rect,
        };
        assert_eq!(source(&editor), mark);
        for clockwise in [true, false] {
            let mut turned = editor.clone();
            for turns in 1..=4 {
                turned.rotate(clockwise);
                assert_eq!(source(&turned), mark, "{turns} turn(s), clockwise={clockwise}");
            }
            assert_eq!(turned.job(None).marks.len(), 1);
        }
        let mut half = editor.clone();
        half.rotate(true);
        half.rotate(true);
        assert_eq!(half.marks()[0].rect, Rect { x: 200 - 10 - 30, y: 100 - 20 - 40, width: 30, height: 40 });
    }

    #[test]
    fn the_job_carries_the_colour_and_thickness_of_each_mark() {
        let mut editor = Editor::new(4000, 3000);
        editor.add_square(Rect { x: 100, y: 100, width: 500, height: 400 }, [1, 2, 3], Weight::Thick);
        let job = editor.job(Some(1280));
        assert_eq!(job.marks, vec![Mark::Square { rect: Rect { x: 100, y: 100, width: 500, height: 400 }, color: [1, 2, 3], stroke: 48 }]);
        assert_eq!(job.max_edge, Some(1280));
        assert!(crate::media_authoring::plan(
            &job,
            &crate::media_authoring::SourceInfo { kind: crate::media_authoring::Kind::Image, width: 4000, height: 3000, duration_ms: None, bytes: 1000 },
            &crate::media_authoring::Limits::default()
        )
        .is_ok());
    }

    #[test]
    fn the_colour_and_thickness_pickers_offer_distinct_named_choices() {
        let names: Vec<&str> = INKS.iter().map(|(name, _)| *name).collect();
        assert_eq!(names, ["Red", "Yellow", "Green", "Blue", "White", "Black"]);
        let mut colours: Vec<[u8; 3]> = INKS.iter().map(|(_, colour)| *colour).collect();
        colours.sort();
        colours.dedup();
        assert_eq!(colours.len(), INKS.len(), "no two names share a colour");
        let weights: Vec<Weight> = WEIGHTS.iter().map(|(_, weight)| *weight).collect();
        assert_eq!(weights, [Weight::Thin, Weight::Medium, Weight::Thick]);
    }
}
