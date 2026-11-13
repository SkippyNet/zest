//! The application mark rendered to pixels: an orange slice with one segment
//! pulled out, the way a pie chart sets one share apart. The loose segment is
//! the space a clean gives back.
//!
//! Used for the window icon, the mark in the navigation rail and to generate
//! `assets/icon.ico` and `assets/logo.png`.

/// Renders the mark as straight (non-premultiplied) RGBA, `size` x `size`.
pub fn render(size: u32) -> Vec<u8> {
    const SAMPLES: u32 = 4;
    let mut pixels = vec![0u8; (size * size * 4) as usize];
    let geometry = Geometry::for_size(size as f32);

    for y in 0..size {
        for x in 0..size {
            let mut acc = [0.0f32; 4];
            for sy in 0..SAMPLES {
                for sx in 0..SAMPLES {
                    let px = x as f32 + (sx as f32 + 0.5) / SAMPLES as f32;
                    let py = y as f32 + (sy as f32 + 0.5) / SAMPLES as f32;
                    if let Some(rgb) = geometry.shade(px, py) {
                        acc[0] += rgb[0];
                        acc[1] += rgb[1];
                        acc[2] += rgb[2];
                        acc[3] += 1.0;
                    }
                }
            }
            let covered = acc[3];
            if covered > 0.0 {
                let i = ((y * size + x) * 4) as usize;
                pixels[i] = (acc[0] / covered).round().clamp(0.0, 255.0) as u8;
                pixels[i + 1] = (acc[1] / covered).round().clamp(0.0, 255.0) as u8;
                pixels[i + 2] = (acc[2] / covered).round().clamp(0.0, 255.0) as u8;
                pixels[i + 3] = (covered / (SAMPLES * SAMPLES) as f32 * 255.0).round() as u8;
            }
        }
    }
    pixels
}

fn mix(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    let t = t.clamp(0.0, 1.0);
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

/// Number of segments in the slice.
const SEGMENTS: f32 = 8.0;
/// Angle covered by one segment, in degrees.
const SEGMENT_SPAN: f32 = 360.0 / SEGMENTS;
/// Direction the loose segment points in: up and to the right.
const LOOSE_DIRECTION: f32 = -45.0;

const RIND_SHADE: [f32; 3] = [226.0, 106.0, 4.0];
const RIND_LIGHT: [f32; 3] = [255.0, 158.0, 44.0];
const PITH: [f32; 3] = [255.0, 244.0, 222.0];
const FLESH_CORE: [f32; 3] = [255.0, 208.0, 112.0];
const FLESH_EDGE: [f32; 3] = [255.0, 146.0, 30.0];

/// Sizes of the parts of the mark for one output size.
struct Geometry {
    centre: (f32, f32),
    radius: f32,
    /// Outer edge of the pale ring under the rind.
    pith_outer: f32,
    /// Inner edge of that ring, where the flesh begins.
    pith_inner: f32,
    /// Radius of the pale core in the middle.
    core: f32,
    /// Half the width of the membranes between segments.
    membrane: f32,
    /// How far the loose segment is pulled out.
    pull: f32,
}

impl Geometry {
    fn for_size(size: f32) -> Geometry {
        let radius = size * 0.415;
        // Small icons keep their lines at least about a pixel wide.
        let rind = (radius * 0.13).max(1.0);
        let ring = (radius * 0.075).max(0.8);
        Geometry {
            // Slightly down and left, leaving room for the loose segment.
            centre: (size * 0.465, size * 0.535),
            radius,
            pith_outer: radius - rind,
            pith_inner: radius - rind - ring,
            core: radius * 0.1,
            membrane: (radius * 0.034).max(0.45),
            pull: radius * 0.14,
        }
    }

    /// Colour of the mark at a point, or `None` outside it.
    fn shade(&self, px: f32, py: f32) -> Option<[f32; 3]> {
        let (dx, dy) = (px - self.centre.0, py - self.centre.1);
        let dist = (dx * dx + dy * dy).sqrt();

        // The slice itself, minus the segment that was pulled out.
        if dist <= self.radius && (dist < self.core || !in_loose_span(dx, dy)) {
            return Some(self.slice_colour(dx, dy, dist));
        }

        // The loose segment: the same slice, moved along its own direction.
        let (sin, cos) = LOOSE_DIRECTION.to_radians().sin_cos();
        let (qx, qy) = (dx - cos * self.pull, dy - sin * self.pull);
        let q = (qx * qx + qy * qy).sqrt();
        if q <= self.radius && q >= self.core && in_loose_span(qx, qy) {
            return Some(self.slice_colour(qx, qy, q));
        }
        None
    }

    /// Colour inside a whole slice centred on the origin.
    fn slice_colour(&self, dx: f32, dy: f32, dist: f32) -> [f32; 3] {
        if dist > self.pith_outer {
            // The rind, lit from the top left.
            let light = ((-dx - dy) / (self.radius * 2.0) + 0.5).clamp(0.0, 1.0);
            return mix(RIND_SHADE, RIND_LIGHT, light);
        }
        if dist > self.pith_inner || dist < self.core {
            return PITH;
        }
        // Membranes run along the boundaries between segments.
        let angle = dy.atan2(dx).to_degrees();
        let boundary = segment_boundary_near(angle);
        let across = dist * (angle - boundary).to_radians().sin().abs();
        if across < self.membrane {
            return PITH;
        }
        let outward = (dist - self.core) / (self.pith_inner - self.core).max(1.0);
        mix(FLESH_CORE, FLESH_EDGE, outward.powf(0.85))
    }
}

/// The segment boundary closest to `angle` (degrees).
fn segment_boundary_near(angle: f32) -> f32 {
    let first = LOOSE_DIRECTION - SEGMENT_SPAN / 2.0;
    first + ((angle - first) / SEGMENT_SPAN).round() * SEGMENT_SPAN
}

/// Whether a direction falls inside the span of the loose segment.
fn in_loose_span(dx: f32, dy: f32) -> bool {
    let angle = dy.atan2(dx).to_degrees();
    let off = (angle - LOOSE_DIRECTION + 180.0).rem_euclid(360.0) - 180.0;
    off.abs() <= SEGMENT_SPAN / 2.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixel(pixels: &[u8], size: u32, x: f32, y: f32) -> [u8; 4] {
        let i = ((y as u32 * size + x as u32) * 4) as usize;
        [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
    }

    #[test]
    fn renders_an_orange_slice_on_a_transparent_field() {
        let size = 64;
        let pixels = render(size);
        assert_eq!(pixels.len(), (size * size * 4) as usize);
        let s = size as f32;
        assert_eq!(
            pixel(&pixels, size, 0.0, 0.0)[3],
            0,
            "corners are transparent"
        );
        assert_eq!(
            pixel(&pixels, size, 0.0, s - 1.0)[3],
            0,
            "corners are transparent"
        );

        // The middle of a segment on the lower left is solid orange flesh.
        let geometry = Geometry::for_size(s);
        let (cx, cy) = geometry.centre;
        let reach = geometry.radius * 0.5;
        let flesh = pixel(&pixels, size, cx - reach * 0.38, cy + reach * 0.92);
        assert_eq!(flesh[3], 255, "the flesh is opaque");
        assert!(
            flesh[0] > 230 && flesh[1] > 120 && flesh[2] < 140,
            "the flesh is orange: {flesh:?}"
        );
    }

    #[test]
    fn one_segment_is_pulled_out_of_the_slice() {
        let size = 256;
        let pixels = render(size);
        let geometry = Geometry::for_size(size as f32);
        let (cx, cy) = geometry.centre;
        let (sin, cos) = LOOSE_DIRECTION.to_radians().sin_cos();

        // Just outside the core, in the loose direction, the slice is open.
        let gap = geometry.core + geometry.pull * 0.5;
        assert_eq!(
            pixel(&pixels, size, cx + cos * gap, cy + sin * gap)[3],
            0,
            "a gap opens where the segment left"
        );
        // The segment itself reaches past the rim of the slice.
        let beyond = geometry.radius + geometry.pull * 0.5;
        assert_eq!(
            pixel(&pixels, size, cx + cos * beyond, cy + sin * beyond)[3],
            255,
            "the loose segment sits outside the rim"
        );
        // The opposite side ends at the rim.
        assert_eq!(
            pixel(&pixels, size, cx - cos * beyond, cy - sin * beyond)[3],
            0,
            "nothing is drawn beyond the rim elsewhere"
        );
    }

    #[test]
    fn every_icon_size_stays_inside_its_square() {
        for size in [16u32, 20, 24, 32, 48, 256] {
            let pixels = render(size);
            let edge_alpha: u32 = (0..size)
                .map(|i| {
                    u32::from(pixels[(i * 4 + 3) as usize])
                        + u32::from(pixels[(((size - 1) * size + i) * 4 + 3) as usize])
                        + u32::from(pixels[((i * size) * 4 + 3) as usize])
                        + u32::from(pixels[((i * size + size - 1) * 4 + 3) as usize])
                })
                .sum();
            assert_eq!(edge_alpha, 0, "size {size} touches the edge of its square");
        }
    }
}
