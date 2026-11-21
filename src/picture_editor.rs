//! The geometry behind the picture editor: a crop rectangle over a possibly rotated picture, with
//! aspect-ratio locking, handle dragging and keyboard nudging. It is pure arithmetic, so it is
//! tested exhaustively without a display, and every operation keeps the crop inside the picture.
//!
//! Coordinates: the crop is kept in *view* pixels, the picture as the user currently sees it
//! (after rotation). `job()` converts it to the source-pixel crop plus rotation that the sandboxed
//! helper applies, which crops first and rotates second (see `media_authoring`).
use crate::media_authoring::{encode_mask, Job, Mark, Point, Rect, Rotation, Shape};
use std::rc::Rc;

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
    /// An exact thickness in picture pixels, as chosen with the size slider.
    Px(u32),
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

/// Thickest line, in picture pixels (the helper refuses more).
pub const MAX_STROKE: u32 = 128;
/// Most marks, and most path points over all of them, one picture may carry (the helper refuses
/// more).
pub const MAX_MARKS: usize = 32;
pub const MAX_POINTS: usize = 1024;
/// Most points one freehand stroke keeps; a longer one is thinned evenly.
pub const MAX_PATH_POINTS: usize = 256;
/// Smallest mark, in view pixels. A click that barely moved is not a shape.
pub const MIN_MARK: u32 = 8;

/// What the user can draw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Square,
    Circle,
    Arrow,
    Freehand,
    Text,
}

/// Text already drawn into a bitmap by the editor: `alpha` is the coverage of each of
/// `width * height` pixels, row by row, with the text upright.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextBitmap {
    pub width: u32,
    pub height: u32,
    pub alpha: Rc<[u8]>,
}

/// Longest text a mark can hold, in characters.
pub const MAX_TEXT_CHARS: usize = 200;
/// Most bitmap pixels in one text mark and in all of them (the helper refuses more).
pub const MAX_TEXT_PIXELS: u64 = 4_000_000;
pub const MAX_TEXT_TOTAL: u64 = 8_000_000;

/// `alpha` (`width` x `height`) turned clockwise by `turns` quarter turns, with its new size.
pub fn rotate_alpha(alpha: &[u8], width: u32, height: u32, turns: u8) -> (u32, u32, Vec<u8>) {
    let (w, h) = (width as usize, height as usize);
    let mut current = (width, height, alpha.to_vec());
    for _ in 0..(turns % 4) {
        let (cw, ch, data) = (current.0 as usize, current.1 as usize, &current.2);
        let mut turned = vec![0u8; data.len()];
        // A clockwise turn sends (x, y) to (ch - 1 - y, x) in a picture that is now ch wide.
        for y in 0..ch {
            for x in 0..cw {
                turned[x * ch + (ch - 1 - y)] = data[y * cw + x];
            }
        }
        current = (current.1, current.0, turned);
    }
    let _ = (w, h);
    current
}

/// The geometry of a mark, in view pixels, so it turns with the picture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ViewShape {
    Rect(Shape, Rect),
    Arrow(Point, Point),
    Freehand(Vec<Point>),
    /// Text: where it sits in the view now, the words (for showing), the upright bitmap, and how
    /// many clockwise quarter turns take that bitmap to the source picture's orientation. The
    /// turns are fixed when the text is placed; turning the picture later moves the box only.
    Text { rect: Rect, text: String, bitmap: TextBitmap, source_turns: u8 },
}

/// Something the user drew.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewMark {
    pub shape: ViewShape,
    pub color: [u8; 3],
    pub stroke: u32,
    /// For a freehand line drawn with a pen: how hard it pressed at each point, 1 to 255. Empty
    /// for a mouse or any other shape, which draw one width all along.
    pub pressure: Vec<u8>,
}
impl ViewMark {
    /// The smallest rectangle that holds the mark.
    pub fn bounds(&self) -> Rect {
        let around = |points: &[Point]| {
            let (x0, x1) = points.iter().fold((u32::MAX, 0), |(lo, hi), p| (lo.min(p.0), hi.max(p.0)));
            let (y0, y1) = points.iter().fold((u32::MAX, 0), |(lo, hi), p| (lo.min(p.1), hi.max(p.1)));
            Rect { x: x0, y: y0, width: x1 - x0, height: y1 - y0 }
        };
        match &self.shape {
            ViewShape::Rect(_, rect) => *rect,
            ViewShape::Arrow(a, b) => around(&[*a, *b]),
            ViewShape::Freehand(points) if points.is_empty() => Rect::default(),
            ViewShape::Freehand(points) => around(points),
            ViewShape::Text { rect, .. } => *rect,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Editor {
    source: (u32, u32),
    rotation: Rotation,
    /// In view pixels.
    rect: Rect,
    aspect: Aspect,
    marks: Vec<ViewMark>,
    /// Marks taken back, newest last, in the same view pixels as `marks`.
    undone: Vec<ViewMark>,
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

/// Carry a point along with the picture when it turns a quarter, `(width, height)` being the size
/// of the picture *before* the turn. Points are positions on the pixel grid, so the far edge maps
/// to the far edge.
fn turn_point((x, y): Point, (width, height): (u32, u32), clockwise: bool) -> Point {
    if clockwise {
        (height - y.min(height), x)
    } else {
        (y, width - x.min(width))
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
            undone: Vec::new(),
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
            Weight::Px(pixels) => return pixels.clamp(1, MAX_STROKE),
        };
        (longest * per_mille / 1000).clamp(2, MAX_STROKE)
    }

    /// Add a square over `rect` (view pixels, clamped into the picture). Refused, returning false,
    /// when it is too small or the picture already has `MAX_MARKS`.
    pub fn add_square(&mut self, rect: Rect, color: [u8; 3], weight: Weight) -> bool {
        self.add_mark(Shape::Square, rect, color, weight)
    }

    /// Like `add_square`, for a circle (the ellipse inscribed in `rect`).
    pub fn add_circle(&mut self, rect: Rect, color: [u8; 3], weight: Weight) -> bool {
        self.add_mark(Shape::Circle, rect, color, weight)
    }

    pub fn add_mark(&mut self, shape: Shape, rect: Rect, color: [u8; 3], weight: Weight) -> bool {
        let (view_w, view_h) = self.view_size();
        let x = rect.x.min(view_w.saturating_sub(1));
        let y = rect.y.min(view_h.saturating_sub(1));
        let rect = Rect { x, y, width: rect.width.min(view_w - x), height: rect.height.min(view_h - y) };
        if self.marks.len() >= MAX_MARKS || rect.width < MIN_MARK.min(view_w) || rect.height < MIN_MARK.min(view_h) {
            return false;
        }
        let stroke = self.stroke_for(weight);
        self.add(ViewMark { shape: ViewShape::Rect(shape, rect), color, stroke, pressure: vec![] });
        true
    }

    fn clamp_point(&self, (x, y): Point) -> Point {
        let (view_w, view_h) = self.view_size();
        (x.min(view_w), y.min(view_h))
    }

    fn points_used(&self) -> usize {
        self.marks
            .iter()
            .map(|mark| match &mark.shape {
                ViewShape::Rect(..) | ViewShape::Text { .. } => 0,
                ViewShape::Arrow(..) => 2,
                ViewShape::Freehand(points) => points.len(),
            })
            .sum()
    }

    fn text_pixels_used(&self) -> u64 {
        self.marks
            .iter()
            .map(|mark| match &mark.shape {
                ViewShape::Text { bitmap, .. } => u64::from(bitmap.width) * u64::from(bitmap.height),
                _ => 0,
            })
            .sum()
    }

    /// Clockwise quarter turns the picture is shown at.
    fn quarter_turns(&self) -> u8 {
        match self.rotation {
            Rotation::None => 0,
            Rotation::Clockwise90 => 1,
            Rotation::Half => 2,
            Rotation::Counterclockwise90 => 3,
        }
    }

    /// Place `text`, drawn as `bitmap`, with its top-left corner at `at` (view pixels). The box is
    /// moved to stay inside the picture. Refused, returning false, when the words are empty or
    /// too long, the bitmap is empty or does not match its pixels, it is too large for the
    /// picture or the helper's limits, or the picture already has `MAX_MARKS`.
    pub fn add_text(&mut self, at: Point, text: &str, bitmap: TextBitmap, color: [u8; 3]) -> bool {
        let words: String = text.chars().filter(|c| !c.is_control()).collect();
        let (view_w, view_h) = self.view_size();
        let pixels = u64::from(bitmap.width) * u64::from(bitmap.height);
        if words.trim().is_empty()
            || words.chars().count() > MAX_TEXT_CHARS
            || bitmap.width == 0
            || bitmap.height == 0
            || bitmap.alpha.len() as u64 != pixels
            || bitmap.width > view_w
            || bitmap.height > view_h
            || pixels > MAX_TEXT_PIXELS
            || self.text_pixels_used() + pixels > MAX_TEXT_TOTAL
            || self.marks.len() >= MAX_MARKS
        {
            return false;
        }
        let rect = Rect {
            x: at.0.min(view_w - bitmap.width),
            y: at.1.min(view_h - bitmap.height),
            width: bitmap.width,
            height: bitmap.height,
        };
        let source_turns = (4 - self.quarter_turns()) % 4;
        self.add(ViewMark { shape: ViewShape::Text { rect, text: words, bitmap, source_turns }, color, stroke: 1, pressure: vec![] });
        true
    }

    /// Add an arrow from `from` to `to` (view pixels, clamped into the picture). Refused when it
    /// is shorter than `MIN_MARK` or the picture is full.
    pub fn add_arrow(&mut self, from: Point, to: Point, color: [u8; 3], weight: Weight) -> bool {
        let (from, to) = (self.clamp_point(from), self.clamp_point(to));
        let (dx, dy) = (i64::from(to.0) - i64::from(from.0), i64::from(to.1) - i64::from(from.1));
        if self.marks.len() >= MAX_MARKS
            || self.points_used() + 2 > MAX_POINTS
            || dx * dx + dy * dy < i64::from(MIN_MARK) * i64::from(MIN_MARK)
        {
            return false;
        }
        let stroke = self.stroke_for(weight);
        self.add(ViewMark { shape: ViewShape::Arrow(from, to), color, stroke, pressure: vec![] });
        true
    }

    /// Add a freehand stroke through `points` (view pixels, clamped into the picture). Repeated
    /// points collapse and a long stroke is thinned to `MAX_PATH_POINTS`, keeping both ends.
    /// Refused when it never moves `MIN_MARK` away from where it began or the picture is full.
    pub fn add_path(&mut self, points: &[Point], color: [u8; 3], weight: Weight) -> bool {
        self.add_pressured_path(points, &[], color, weight)
    }

    /// Like `add_path` for a pen: `pressure` has one value (1 to 255) per point, or is empty for
    /// a line of one width. A repeated point keeps its first pressure, and thinning keeps the
    /// pressure of the points it keeps. A list that does not match the points is refused.
    pub fn add_pressured_path(&mut self, points: &[Point], pressure: &[u8], color: [u8; 3], weight: Weight) -> bool {
        if !pressure.is_empty() && (pressure.len() != points.len() || pressure.contains(&0)) {
            return false;
        }
        let mut path: Vec<Point> = Vec::with_capacity(points.len().min(MAX_PATH_POINTS * 2));
        let mut levels: Vec<u8> = Vec::new();
        for (index, point) in points.iter().map(|point| self.clamp_point(*point)).enumerate() {
            if path.last() != Some(&point) {
                path.push(point);
                if let Some(level) = pressure.get(index) {
                    levels.push(*level);
                }
            }
        }
        let Some(&first) = path.first() else { return false };
        let reach = path.iter().any(|p| {
            let (dx, dy) = (i64::from(p.0) - i64::from(first.0), i64::from(p.1) - i64::from(first.1));
            dx * dx + dy * dy >= i64::from(MIN_MARK) * i64::from(MIN_MARK)
        });
        if !reach {
            return false;
        }
        if path.len() > MAX_PATH_POINTS {
            let last = path.len() - 1;
            let keep: Vec<usize> = (0..MAX_PATH_POINTS).map(|i| i * last / (MAX_PATH_POINTS - 1)).collect();
            path = keep.iter().map(|i| path[*i]).collect();
            if !levels.is_empty() {
                levels = keep.iter().map(|i| levels[*i]).collect();
            }
        }
        if self.marks.len() >= MAX_MARKS || self.points_used() + path.len() > MAX_POINTS {
            return false;
        }
        let stroke = self.stroke_for(weight);
        self.add(ViewMark { shape: ViewShape::Freehand(path), color, stroke, pressure: levels });
        true
    }

    /// Keep a new mark. Drawing again ends the chance to redo what was undone.
    fn add(&mut self, mark: ViewMark) {
        self.marks.push(mark);
        self.undone.clear();
    }

    /// Mark the crop area as it is now, which places a mark without a pointer: a square or circle
    /// around it, an arrow across it from corner to corner, or a stroke along its edge.
    pub fn mark_crop(&mut self, kind: Kind, color: [u8; 3], weight: Weight) -> bool {
        let r = self.rect;
        let (right, bottom) = (r.x + r.width, r.y + r.height);
        match kind {
            Kind::Square => self.add_mark(Shape::Square, r, color, weight),
            Kind::Circle => self.add_mark(Shape::Circle, r, color, weight),
            Kind::Arrow => self.add_arrow((r.x, r.y), (right, bottom), color, weight),
            Kind::Freehand => self.add_path(&[(r.x, r.y), (right, r.y), (right, bottom), (r.x, bottom), (r.x, r.y)], color, weight),
            // Words need typing, so there is nothing to place on the crop by itself.
            Kind::Text => false,
        }
    }

    /// Take back the newest mark; it can be brought back with `redo_mark` until something new is
    /// drawn.
    pub fn undo_mark(&mut self) -> bool {
        match self.marks.pop() {
            Some(mark) => {
                self.undone.push(mark);
                true
            }
            None => false,
        }
    }

    /// Bring back the mark most recently taken back.
    pub fn redo_mark(&mut self) -> bool {
        if self.marks.len() >= MAX_MARKS {
            return false;
        }
        match self.undone.pop() {
            Some(mark) => {
                self.marks.push(mark);
                true
            }
            None => false,
        }
    }

    /// Whether `redo_mark` has anything to bring back.
    pub fn can_redo(&self) -> bool {
        !self.undone.is_empty()
    }

    /// Turn the picture a quarter; the crop and the marks turn with it.
    pub fn rotate(&mut self, clockwise: bool) {
        let before = self.view_size();
        for mark in self.marks.iter_mut().chain(self.undone.iter_mut()) {
            match &mut mark.shape {
                ViewShape::Rect(_, rect) | ViewShape::Text { rect, .. } => *rect = turn_rect(*rect, before, clockwise),
                ViewShape::Arrow(a, b) => {
                    *a = turn_point(*a, before, clockwise);
                    *b = turn_point(*b, before, clockwise);
                }
                ViewShape::Freehand(points) => {
                    for point in points {
                        *point = turn_point(*point, before, clockwise);
                    }
                }
            }
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

    /// The same for a point.
    fn point_to_source(&self, point: Point) -> Point {
        let view = self.view_size();
        match self.rotation {
            Rotation::None => point,
            Rotation::Clockwise90 => turn_point(point, view, false),
            Rotation::Counterclockwise90 => turn_point(point, view, true),
            Rotation::Half => turn_point(turn_point(point, view, true), (view.1, view.0), true),
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
            .map(|mark| match &mark.shape {
                ViewShape::Rect(shape, rect) => Mark::of(*shape, self.to_source(*rect), mark.color, mark.stroke),
                ViewShape::Arrow(a, b) => Mark::Arrow {
                    from: self.point_to_source(*a),
                    to: self.point_to_source(*b),
                    color: mark.color,
                    stroke: mark.stroke,
                },
                ViewShape::Text { rect, bitmap, source_turns, .. } => {
                    let (width, height, alpha) = rotate_alpha(&bitmap.alpha, bitmap.width, bitmap.height, *source_turns);
                    let at = self.to_source(*rect);
                    debug_assert_eq!((at.width, at.height), (width, height));
                    Mark::Text { rect: at, color: mark.color, mask: encode_mask(&alpha) }
                }
                ViewShape::Freehand(points) => Mark::Freehand {
                    points: points.iter().map(|point| self.point_to_source(*point)).collect(),
                    color: mark.color,
                    stroke: mark.stroke,
                    pressure: mark.pressure.clone(),
                },
            })
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
        assert_eq!(editor.marks()[0].bounds(), Rect { x: 150, y: 80, width: 50, height: 20 });
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
        assert!(editor.mark_crop(Kind::Square, RED, Weight::Medium));
        assert_eq!(editor.marks()[0].bounds(), editor.rect());
        editor.set_rect(0, 0, 50, 50);
        assert_eq!(editor.marks()[0].bounds(), Rect { x: 20, y: 10, width: 100, height: 60 }, "the mark stays put");
    }

    #[test]
    fn marks_reach_the_helper_in_source_pixels_for_every_rotation() {
        let mark = Rect { x: 10, y: 20, width: 30, height: 40 };
        let mut editor = Editor::new(200, 100);
        editor.add_square(mark, RED, Weight::Thin);
        let source = |editor: &Editor| match editor.job(None).marks[0] {
            Mark::Square { rect, .. } => rect,
            _ => unreachable!("a square stays a square"),
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
        assert_eq!(half.marks()[0].bounds(), Rect { x: 200 - 10 - 30, y: 100 - 20 - 40, width: 30, height: 40 });
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

    fn shape_of(editor: &Editor, index: usize) -> ViewShape {
        editor.marks()[index].shape.clone()
    }

    #[test]
    fn a_circle_is_a_rectangle_mark_with_the_circle_shape() {
        let mut editor = Editor::new(200, 100);
        assert!(editor.add_circle(Rect { x: 10, y: 10, width: 60, height: 40 }, RED, Weight::Thin));
        assert_eq!(shape_of(&editor, 0), ViewShape::Rect(Shape::Circle, Rect { x: 10, y: 10, width: 60, height: 40 }));
        assert!(!editor.add_circle(Rect { x: 10, y: 10, width: 3, height: 40 }, RED, Weight::Thin), "too narrow");
        assert!(matches!(editor.job(None).marks[0], Mark::Circle { .. }));
    }

    #[test]
    fn an_arrow_is_clamped_and_must_have_a_length() {
        let mut editor = Editor::new(200, 100);
        assert!(editor.add_arrow((10, 10), (90, 40), RED, Weight::Medium));
        assert_eq!(shape_of(&editor, 0), ViewShape::Arrow((10, 10), (90, 40)));
        assert!(editor.add_arrow((10, 10), (9999, 9999), RED, Weight::Medium));
        assert_eq!(shape_of(&editor, 1), ViewShape::Arrow((10, 10), (200, 100)), "the tip stops at the far corner");
        assert!(!editor.add_arrow((50, 50), (50, 50), RED, Weight::Thin), "no length");
        assert!(!editor.add_arrow((50, 50), (50 + MIN_MARK - 1, 50), RED, Weight::Thin), "a twitch");
        assert!(editor.add_arrow((50, 50), (50 + MIN_MARK, 50), RED, Weight::Thin));
        assert_eq!(editor.marks().len(), 3);
    }

    #[test]
    fn a_freehand_stroke_collapses_repeats_and_must_travel() {
        let mut editor = Editor::new(200, 100);
        assert!(editor.add_path(&[(10, 10), (10, 10), (30, 10), (30, 10), (30, 40)], RED, Weight::Thin));
        assert_eq!(shape_of(&editor, 0), ViewShape::Freehand(vec![(10, 10), (30, 10), (30, 40)]));
        assert!(!editor.add_path(&[], RED, Weight::Thin));
        assert!(!editor.add_path(&[(5, 5)], RED, Weight::Thin), "one point is a click");
        assert!(!editor.add_path(&[(5, 5), (6, 6), (5, 6), (6, 5)], RED, Weight::Thin), "a wobble in place");
        assert!(editor.add_path(&[(5, 5), (900, 900)], RED, Weight::Thin));
        assert_eq!(shape_of(&editor, 1), ViewShape::Freehand(vec![(5, 5), (200, 100)]), "clamped into the picture");
    }

    #[test]
    fn a_long_stroke_is_thinned_keeping_both_ends_and_the_order() {
        let mut editor = Editor::new(5000, 100);
        let long: Vec<Point> = (0..3000).map(|i| (i, i % 100)).collect();
        assert!(editor.add_path(&long, RED, Weight::Thin));
        let ViewShape::Freehand(kept) = shape_of(&editor, 0) else { panic!("a stroke") };
        assert_eq!(kept.len(), MAX_PATH_POINTS);
        assert_eq!((kept[0], *kept.last().unwrap()), (long[0], long[2999]));
        assert!(kept.windows(2).all(|pair| pair[0].0 < pair[1].0), "still in drawing order");
    }

    #[test]
    fn path_points_are_budgeted_over_the_whole_picture() {
        assert_eq!(MAX_POINTS, crate::media_authoring::Limits::default().max_points);
        assert_eq!(MAX_PATH_POINTS, crate::media_authoring::Limits::default().max_path_points);
        let mut editor = Editor::new(5000, 100);
        let stroke: Vec<Point> = (0..MAX_PATH_POINTS as u32).map(|i| (i * 10, i % 90)).collect();
        for _ in 0..4 {
            assert!(editor.add_path(&stroke, RED, Weight::Thin));
        }
        assert!(!editor.add_path(&[(0, 0), (50, 50)], RED, Weight::Thin), "the point budget is spent");
        assert!(!editor.add_arrow((0, 0), (50, 50), RED, Weight::Thin));
        assert!(editor.add_circle(Rect { x: 0, y: 0, width: 50, height: 50 }, RED, Weight::Thin), "shapes without points still fit");
        assert!(editor.undo_mark() && editor.undo_mark(), "undoing frees the budget");
        assert!(editor.add_path(&[(0, 0), (50, 50)], RED, Weight::Thin));
    }

    #[test]
    fn marking_the_crop_gives_each_kind_a_sensible_shape() {
        let mut editor = Editor::new(200, 100);
        editor.set_rect(20, 10, 100, 60);
        for kind in [Kind::Square, Kind::Circle, Kind::Arrow, Kind::Freehand] {
            assert!(editor.mark_crop(kind, RED, Weight::Thin), "{kind:?}");
        }
        assert_eq!(shape_of(&editor, 0), ViewShape::Rect(Shape::Square, editor.rect()));
        assert_eq!(shape_of(&editor, 1), ViewShape::Rect(Shape::Circle, editor.rect()));
        assert_eq!(shape_of(&editor, 2), ViewShape::Arrow((20, 10), (120, 70)), "corner to corner");
        assert_eq!(
            shape_of(&editor, 3),
            ViewShape::Freehand(vec![(20, 10), (120, 10), (120, 70), (20, 70), (20, 10)]),
            "round the edge"
        );
        for mark in editor.marks() {
            assert_eq!(mark.bounds(), editor.rect());
        }
    }

    #[test]
    fn every_kind_of_mark_turns_with_the_picture_and_comes_home_after_four_turns() {
        for clockwise in [true, false] {
            let mut editor = Editor::new(200, 100);
            editor.add_mark(Shape::Circle, Rect { x: 10, y: 20, width: 30, height: 40 }, RED, Weight::Thin);
            editor.add_arrow((15, 25), (150, 80), RED, Weight::Thin);
            editor.add_path(&[(0, 0), (200, 0), (200, 100), (7, 99)], RED, Weight::Thin);
            let before: Vec<ViewShape> = editor.marks().iter().map(|m| m.shape.clone()).collect();
            for _ in 0..4 {
                editor.rotate(clockwise);
            }
            let after: Vec<ViewShape> = editor.marks().iter().map(|m| m.shape.clone()).collect();
            assert_eq!(before, after, "clockwise={clockwise}");
        }
    }

    #[test]
    fn a_turned_arrow_and_stroke_keep_their_place_on_the_picture() {
        let mut editor = Editor::new(200, 100);
        editor.add_arrow((10, 20), (150, 80), RED, Weight::Thin);
        editor.add_path(&[(0, 0), (200, 100)], RED, Weight::Thin);
        editor.rotate(true);
        // A quarter clockwise on a 200x100 picture: (x, y) goes to (100 - y, x).
        assert_eq!(shape_of(&editor, 0), ViewShape::Arrow((80, 10), (20, 150)));
        assert_eq!(shape_of(&editor, 1), ViewShape::Freehand(vec![(100, 0), (0, 200)]));
        editor.rotate(false);
        assert_eq!(shape_of(&editor, 0), ViewShape::Arrow((10, 20), (150, 80)));
    }

    #[test]
    fn points_reach_the_helper_in_source_pixels_for_every_rotation() {
        let mut editor = Editor::new(200, 100);
        editor.add_arrow((10, 20), (150, 80), RED, Weight::Thin);
        editor.add_path(&[(3, 4), (199, 4), (199, 99)], RED, Weight::Thin);
        let expected = editor.job(None).marks;
        for clockwise in [true, false] {
            let mut turned = editor.clone();
            for turns in 1..=4 {
                turned.rotate(clockwise);
                assert_eq!(turned.job(None).marks, expected, "{turns} turn(s), clockwise={clockwise}");
            }
        }
        assert!(matches!(&expected[0], Mark::Arrow { from: (10, 20), to: (150, 80), .. }));
        assert!(matches!(&expected[1], Mark::Freehand { points, .. } if points == &[(3, 4), (199, 4), (199, 99)]));
    }

    #[test]
    fn a_job_with_every_kind_of_mark_is_accepted_by_the_planner() {
        let mut editor = Editor::new(400, 300);
        editor.set_rect(20, 20, 300, 200);
        for kind in [Kind::Square, Kind::Circle, Kind::Arrow, Kind::Freehand] {
            assert!(editor.mark_crop(kind, RED, Weight::Medium));
        }
        editor.rotate(true);
        let info = crate::media_authoring::SourceInfo { kind: crate::media_authoring::Kind::Image, width: 400, height: 300, duration_ms: None, bytes: 1000 };
        let made = crate::media_authoring::plan(&editor.job(Some(1000)), &info, &crate::media_authoring::Limits::default()).unwrap();
        assert_eq!(made.marks.len(), 4);
    }

    #[test]
    fn bounds_cover_every_kind_of_mark() {
        let mut editor = Editor::new(200, 100);
        editor.add_arrow((150, 80), (10, 20), RED, Weight::Thin);
        editor.add_path(&[(30, 90), (60, 10), (90, 50)], RED, Weight::Thin);
        assert_eq!(editor.marks()[0].bounds(), Rect { x: 10, y: 20, width: 140, height: 60 });
        assert_eq!(editor.marks()[1].bounds(), Rect { x: 30, y: 10, width: 60, height: 80 });
        let empty = ViewMark { shape: ViewShape::Freehand(vec![]), color: RED, stroke: 2, pressure: vec![] };
        assert_eq!(empty.bounds(), Rect::default());
    }

    #[test]
    fn an_exact_size_is_used_as_chosen_within_the_helpers_limits() {
        let editor = Editor::new(4000, 3000);
        assert_eq!(editor.stroke_for(Weight::Px(12)), 12);
        assert_eq!(editor.stroke_for(Weight::Px(1)), 1, "a hairline is allowed when asked for");
        assert_eq!(editor.stroke_for(Weight::Px(0)), 1);
        assert_eq!(editor.stroke_for(Weight::Px(9999)), MAX_STROKE);
        assert_eq!(MAX_STROKE, crate::media_authoring::Limits::default().max_stroke);
        let mut editor = Editor::new(400, 300);
        assert!(editor.add_arrow((10, 10), (200, 100), RED, Weight::Px(7)));
        assert_eq!(editor.marks()[0].stroke, 7);
        assert!(editor.add_mark(Shape::Circle, Rect { x: 5, y: 5, width: 100, height: 80 }, RED, Weight::Px(500)));
        assert_eq!(editor.marks()[1].stroke, MAX_STROKE);
    }

    #[test]
    fn an_undone_mark_can_be_brought_back_in_the_order_it_was_taken() {
        let mut editor = Editor::new(200, 100);
        for x in [10, 50, 90] {
            editor.add_square(Rect { x, y: 10, width: 30, height: 30 }, RED, Weight::Thin);
        }
        assert!(!editor.can_redo(), "nothing undone yet");
        assert!(!editor.redo_mark());
        assert!(editor.undo_mark() && editor.undo_mark());
        assert!(editor.can_redo());
        assert_eq!(editor.marks().len(), 1);
        assert!(editor.redo_mark());
        assert_eq!(editor.marks()[1].bounds().x, 50, "the first one undone last comes back first");
        assert!(editor.redo_mark());
        assert_eq!(editor.marks()[2].bounds().x, 90);
        assert!(!editor.can_redo() && !editor.redo_mark(), "everything is back");
    }

    #[test]
    fn drawing_something_new_ends_the_chance_to_redo() {
        for add in [
            Box::new(|e: &mut Editor| e.add_square(Rect { x: 0, y: 0, width: 20, height: 20 }, RED, Weight::Thin)) as Box<dyn Fn(&mut Editor) -> bool>,
            Box::new(|e: &mut Editor| e.add_circle(Rect { x: 0, y: 0, width: 20, height: 20 }, RED, Weight::Thin)),
            Box::new(|e: &mut Editor| e.add_arrow((0, 0), (50, 50), RED, Weight::Thin)),
            Box::new(|e: &mut Editor| e.add_path(&[(0, 0), (50, 50)], RED, Weight::Thin)),
            Box::new(|e: &mut Editor| e.mark_crop(Kind::Square, RED, Weight::Thin)),
        ] {
            let mut editor = Editor::new(200, 100);
            editor.add_square(Rect { x: 10, y: 10, width: 30, height: 30 }, RED, Weight::Thin);
            editor.undo_mark();
            assert!(editor.can_redo());
            assert!(add(&mut editor));
            assert!(!editor.can_redo(), "a new mark clears what could be redone");
            assert!(!editor.redo_mark());
        }
        // A refused mark changes nothing: the redo stays.
        let mut editor = Editor::new(200, 100);
        editor.add_square(Rect { x: 10, y: 10, width: 30, height: 30 }, RED, Weight::Thin);
        editor.undo_mark();
        assert!(!editor.add_square(Rect { x: 0, y: 0, width: 2, height: 2 }, RED, Weight::Thin));
        assert!(editor.can_redo(), "nothing was drawn, so the redo is still there");
    }

    #[test]
    fn reset_forgets_what_could_be_redone_and_a_turn_carries_it_along() {
        let mut editor = Editor::new(200, 100);
        editor.add_arrow((10, 20), (150, 80), RED, Weight::Thin);
        editor.undo_mark();
        editor.rotate(true);
        assert!(editor.redo_mark());
        assert_eq!(shape_of(&editor, 0), ViewShape::Arrow((80, 10), (20, 150)), "turned with the picture while undone");
        assert!(matches!(editor.job(None).marks[0], Mark::Arrow { from: (10, 20), to: (150, 80), .. }), "and still lands in the same place on the source");
        editor.undo_mark();
        editor.reset();
        assert!(!editor.can_redo() && editor.marks().is_empty());
    }

    #[test]
    fn redo_does_not_count_as_a_change_on_its_own() {
        let mut editor = Editor::new(200, 100);
        editor.add_square(Rect { x: 10, y: 10, width: 30, height: 30 }, RED, Weight::Thin);
        editor.undo_mark();
        assert!(editor.is_unchanged(), "an undone mark is not part of the picture");
        editor.redo_mark();
        assert!(!editor.is_unchanged());
    }

    #[test]
    fn a_pen_stroke_keeps_a_pressure_for_each_point_it_keeps() {
        let mut editor = Editor::new(200, 100);
        assert!(editor.add_pressured_path(&[(10, 10), (10, 10), (60, 10), (60, 50)], &[40, 90, 160, 255], RED, Weight::Px(10)));
        assert_eq!(shape_of(&editor, 0), ViewShape::Freehand(vec![(10, 10), (60, 10), (60, 50)]));
        assert_eq!(editor.marks()[0].pressure, [40, 160, 255], "the repeated point keeps its first pressure");
        assert!(matches!(&editor.job(None).marks[0], Mark::Freehand { pressure, .. } if pressure == &[40, 160, 255]));
    }

    #[test]
    fn mismatched_or_zero_pressure_is_refused_and_a_mouse_line_has_none() {
        let mut editor = Editor::new(200, 100);
        assert!(!editor.add_pressured_path(&[(10, 10), (60, 60)], &[100], RED, Weight::Thin));
        assert!(!editor.add_pressured_path(&[(10, 10), (60, 60)], &[100, 0], RED, Weight::Thin));
        assert!(editor.marks().is_empty());
        assert!(editor.add_path(&[(10, 10), (60, 60)], RED, Weight::Thin));
        assert!(editor.marks()[0].pressure.is_empty());
    }

    #[test]
    fn thinning_a_long_pen_stroke_keeps_each_kept_pressure_with_its_point() {
        let mut editor = Editor::new(5000, 100);
        let points: Vec<Point> = (0..3000).map(|i| (i, i % 100)).collect();
        let levels: Vec<u8> = (0..3000).map(|i| (i % 250) as u8 + 1).collect();
        assert!(editor.add_pressured_path(&points, &levels, RED, Weight::Thin));
        let ViewShape::Freehand(kept) = shape_of(&editor, 0) else { panic!("a stroke") };
        let pressure = &editor.marks()[0].pressure;
        assert_eq!((kept.len(), pressure.len()), (MAX_PATH_POINTS, MAX_PATH_POINTS));
        for (point, level) in kept.iter().zip(pressure) {
            assert_eq!(*level, levels[point.0 as usize], "pressure stayed with its point at x={}", point.0);
        }
    }

    fn bitmap(width: u32, height: u32, alpha: &[u8]) -> TextBitmap {
        TextBitmap { width, height, alpha: alpha.to_vec().into() }
    }
    /// An asymmetric 4x3 pattern, so a wrong turn cannot look right.
    fn pattern() -> TextBitmap {
        bitmap(4, 3, &[255, 255, 255, 0, 255, 0, 0, 0, 255, 128, 64, 32])
    }

    #[test]
    fn turning_a_bitmap_a_quarter_at_a_time_matches_turning_it_by_hand() {
        let (w, h, a) = (4, 3, [1u8, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]);
        assert_eq!(rotate_alpha(&a, w, h, 0), (4, 3, a.to_vec()));
        // Clockwise: the left column read bottom to top becomes the top row.
        assert_eq!(rotate_alpha(&a, w, h, 1), (3, 4, vec![9, 5, 1, 10, 6, 2, 11, 7, 3, 12, 8, 4]));
        assert_eq!(rotate_alpha(&a, w, h, 2), (4, 3, vec![12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1]));
        assert_eq!(rotate_alpha(&a, w, h, 3), (3, 4, vec![4, 8, 12, 3, 7, 11, 2, 6, 10, 1, 5, 9]));
        assert_eq!(rotate_alpha(&a, w, h, 4), (4, 3, a.to_vec()), "four turns come home");
        assert_eq!(rotate_alpha(&a, w, h, 5), rotate_alpha(&a, w, h, 1), "turns count modulo four");
        for turns in 0..4 {
            let (tw, th, turned) = rotate_alpha(&a, w, h, turns);
            let (bw, bh, back) = rotate_alpha(&turned, tw, th, 4 - turns);
            assert_eq!((bw, bh, back), (w, h, a.to_vec()), "undoing {turns}");
        }
    }

    #[test]
    fn text_is_placed_kept_inside_and_refused_when_it_cannot_be() {
        let mut editor = Editor::new(100, 60);
        assert!(editor.add_text((10, 5), "Hi", pattern(), RED));
        assert_eq!(editor.marks()[0].bounds(), Rect { x: 10, y: 5, width: 4, height: 3 });
        assert!(editor.add_text((99, 59), "Hi", pattern(), RED));
        assert_eq!(editor.marks()[1].bounds(), Rect { x: 96, y: 57, width: 4, height: 3 }, "moved back inside");
        assert!(!editor.add_text((0, 0), "", pattern(), RED));
        assert!(!editor.add_text((0, 0), "  \t\n", pattern(), RED), "only blanks");
        assert!(!editor.add_text((0, 0), "\u{7}\u{1b}", pattern(), RED), "only controls");
        assert!(!editor.add_text((0, 0), &"x".repeat(MAX_TEXT_CHARS + 1), pattern(), RED));
        assert!(editor.add_text((0, 0), &"x".repeat(MAX_TEXT_CHARS), pattern(), RED));
        assert!(!editor.add_text((0, 0), "Hi", bitmap(4, 3, &[1; 11]), RED), "a bitmap that does not match its size");
        assert!(!editor.add_text((0, 0), "Hi", bitmap(0, 3, &[]), RED));
        assert!(!editor.add_text((0, 0), "Hi", bitmap(101, 1, &[1; 101]), RED), "wider than the picture");
        assert!(!editor.add_text((0, 0), "Hi", bitmap(1, 61, &[1; 61]), RED), "taller than the picture");
        assert_eq!(editor.marks().len(), 3);
    }

    #[test]
    fn text_bitmaps_are_budgeted_like_the_helper_budgets_them() {
        assert_eq!((MAX_TEXT_PIXELS, MAX_TEXT_TOTAL), (crate::media_authoring::Limits::default().max_text_pixels, crate::media_authoring::Limits::default().max_text_total));
        let mut editor = Editor::new(3000, 2000);
        let big = |w: u32, h: u32| bitmap(w, h, &vec![1u8; (w * h) as usize]);
        assert!(!editor.add_text((0, 0), "x", big(2001, 2000), RED), "over the per-mark cap");
        assert!(editor.add_text((0, 0), "x", big(2000, 2000), RED));
        assert!(editor.add_text((0, 0), "x", big(2000, 2000), RED));
        assert!(!editor.add_text((0, 0), "x", big(10, 10), RED), "the total is spent");
        editor.undo_mark();
        assert!(editor.add_text((0, 0), "x", big(10, 10), RED), "undoing gives it back");
        let mut many = Editor::new(100, 100);
        for _ in 0..MAX_MARKS {
            assert!(many.add_text((0, 0), "x", bitmap(1, 1, &[255]), RED));
        }
        assert!(!many.add_text((0, 0), "x", bitmap(1, 1, &[255]), RED), "the mark cap");
    }

    #[test]
    fn text_has_nothing_to_place_without_a_pointer_and_does_not_redo_over_new_marks() {
        let mut editor = Editor::new(100, 60);
        assert!(!editor.mark_crop(Kind::Text, RED, Weight::Thin), "words need typing");
        editor.add_text((5, 5), "Hi", pattern(), RED);
        editor.undo_mark();
        assert!(editor.can_redo());
        assert!(editor.add_text((9, 9), "Yo", pattern(), RED));
        assert!(!editor.can_redo(), "new text ends the redo, like any new mark");
    }

    /// The picture as shown: `alpha` (one byte per source pixel) turned by the editor's rotation.
    fn shown_alpha(alpha: &[u8], width: u32, height: u32, rotation: Rotation) -> (u32, u32, Vec<u8>) {
        let turns = match rotation {
            Rotation::None => 0,
            Rotation::Clockwise90 => 1,
            Rotation::Half => 2,
            Rotation::Counterclockwise90 => 3,
        };
        rotate_alpha(alpha, width, height, turns)
    }

    #[test]
    fn text_comes_out_upright_where_it_was_put_whatever_the_picture_was_turned_to() {
        let (source_w, source_h) = (14u32, 9u32);
        for start in 0..4u8 {
            for later in 0..4u8 {
                let mut editor = Editor::new(source_w, source_h);
                for _ in 0..start {
                    editor.rotate(true);
                }
                let (vw, vh) = editor.view_size();
                let bitmap = pattern();
                assert!(editor.add_text((2, 1), "Hi", bitmap.clone(), RED), "start {start}");
                for _ in 0..later {
                    editor.rotate(true);
                }
                let job = editor.job(None);
                // Paint the job on an empty source, then look at it the way the editor shows it.
                let mut rgba = vec![0u8; (source_w * source_h * 4) as usize];
                let info = crate::media_authoring::SourceInfo { kind: crate::media_authoring::Kind::Image, width: source_w, height: source_h, duration_ms: None, bytes: 100 };
                let made = crate::media_authoring::plan(&job, &info, &crate::media_authoring::Limits::default()).expect("the helper would accept it");
                assert!(crate::media_authoring::paint(&mut rgba, source_w, source_h, &made.marks));
                let plane: Vec<u8> = rgba.chunks(4).map(|px| px[3]).collect();
                let (shown_w, shown_h, shown) = shown_alpha(&plane, source_w, source_h, editor.rotation());
                assert_eq!((shown_w, shown_h), editor.view_size());
                // The text, turned by however much the picture turned after it was placed, at its box.
                let (bw, bh, expected) = rotate_alpha(&bitmap.alpha, bitmap.width, bitmap.height, later);
                let rect = editor.marks()[0].bounds();
                assert_eq!((rect.width, rect.height), (bw, bh), "start {start} later {later}");
                for y in 0..shown_h {
                    for x in 0..shown_w {
                        let inside = x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height;
                        let want = if inside { expected[((y - rect.y) * bw + (x - rect.x)) as usize] } else { 0 };
                        assert_eq!(shown[(y * shown_w + x) as usize], want, "start {start} later {later} at ({x},{y}) of {vw}x{vh}");
                    }
                }
            }
        }
    }

    #[test]
    fn two_texts_and_a_line_keep_their_own_places_through_turns_and_undo() {
        let mut editor = Editor::new(40, 30);
        editor.add_text((1, 1), "A", pattern(), RED);
        editor.add_arrow((5, 5), (30, 20), RED, Weight::Px(2));
        editor.rotate(false);
        editor.add_text((3, 2), "B", pattern(), RED);
        editor.undo_mark();
        editor.rotate(true);
        let marks = editor.job(None).marks;
        assert_eq!(marks.len(), 2);
        assert!(matches!(&marks[0], Mark::Text { rect: Rect { x: 1, y: 1, width: 4, height: 3 }, .. }));
        assert!(matches!(&marks[1], Mark::Arrow { from: (5, 5), to: (30, 20), .. }));
    }
}
