//! Original miniature illustrations for the Wave carousel.

use eframe::egui::{Color32, Painter, Pos2, Rect, Shape, Stroke, pos2, vec2};

#[derive(Clone, Copy)]
enum Motif {
    Wave,
    Compass,
    Heart,
    Flame,
    Talk,
    Globe,
    Note,
    Water,
    Sun,
    Drop,
    Bolt,
    Stars,
    Moon,
    Peak,
    Leaf,
    Book,
    Desk,
    Dawn,
    Run,
    Weights,
    Road,
    Route,
    Confetti,
    Guitar,
    Headphones,
    Record,
    Sax,
    Synth,
    Keys,
    Mic,
    Film,
    Cassette,
    Disc,
    Player,
}

fn style(id: Option<&str>) -> (Motif, [u8; 3]) {
    use Motif::*;
    match id {
        None => (Wave, [255, 208, 87]),
        Some("settingDiversity:discover") => (Compass, [255, 170, 90]),
        Some("settingDiversity:favorite") => (Heart, [237, 130, 151]),
        Some("settingDiversity:popular") => (Flame, [249, 141, 96]),
        Some("settingLanguage:russian") => (Talk, [243, 119, 143]),
        Some("settingLanguage:not-russian") => (Globe, [121, 183, 241]),
        Some("settingLanguage:without-words") => (Note, [120, 211, 191]),
        Some("mood:calm") => (Water, [111, 184, 229]),
        Some("mood:happy") => (Sun, [255, 209, 104]),
        Some("mood:sad") => (Drop, [115, 155, 237]),
        Some("mood:energetic") => (Bolt, [250, 188, 73]),
        Some("mood:dream") => (Stars, [180, 157, 242]),
        Some("mood:dark") => (Moon, [170, 145, 226]),
        Some("mood:epic") => (Peak, [247, 139, 123]),
        Some("mood:relaxed") => (Leaf, [146, 207, 149]),
        Some("activity:study-background") => (Book, [141, 184, 241]),
        Some("activity:work-background") => (Desk, [135, 206, 218]),
        Some("activity:wake-up") => (Dawn, [248, 177, 107]),
        Some("activity:run") => (Run, [160, 216, 133]),
        Some("activity:workout") => (Weights, [246, 130, 115]),
        Some("activity:driving") => (Road, [131, 170, 240]),
        Some("activity:road-trip") => (Route, [247, 168, 106]),
        Some("activity:party") => (Confetti, [242, 138, 183]),
        Some("activity:fall-asleep") => (Moon, [123, 169, 231]),
        Some("genre:alternative") => (Guitar, [237, 152, 112]),
        Some("micro-genre:alternative-pop-rock") => (Guitar, [235, 126, 166]),
        Some("micro-genre:alternative-indie-rock") => (Guitar, [153, 167, 240]),
        Some("genre:indie") => (Headphones, [121, 163, 238]),
        Some("genre:phonkgenre") => (Record, [177, 136, 239]),
        Some("genre:ambientgenre") => (Water, [118, 207, 200]),
        Some("genre:postpunk") => (Bolt, [169, 183, 211]),
        Some("genre:jazz") => (Sax, [247, 192, 109]),
        Some("genre:electronics") => (Synth, [144, 191, 240]),
        Some("genre:allrock") => (Guitar, [244, 123, 115]),
        Some("genre:classical") => (Keys, [204, 180, 235]),
        Some("genre:rap") => (Mic, [242, 170, 113]),
        Some("genre:soundtrack") => (Film, [147, 194, 233]),
        Some("epoch:eighties") => (Cassette, [229, 141, 194]),
        Some("epoch:nineties") => (Disc, [171, 207, 237]),
        Some("epoch:zeroes") => (Player, [168, 196, 224]),
        Some(_) => (Note, [166, 181, 222]),
    }
}

struct Art<'a> {
    painter: &'a Painter,
    origin: Pos2,
    unit: f32,
    ink: Color32,
    soft: Color32,
    dark: Color32,
}

impl Art<'_> {
    fn at(&self, [x, y]: [f32; 2]) -> Pos2 {
        self.origin + vec2(x, y) * self.unit
    }

    fn disc(&self, point: [f32; 2], radius: f32, color: Color32) {
        self.painter
            .circle_filled(self.at(point), radius * self.unit, color);
    }

    fn ring(&self, point: [f32; 2], radius: f32, color: Color32) {
        self.painter.circle_stroke(
            self.at(point),
            radius * self.unit,
            Stroke::new(2.4 * self.unit, color),
        );
    }

    fn line(&self, points: &[[f32; 2]], color: Color32) {
        self.painter.add(Shape::line(
            points.iter().map(|&p| self.at(p)).collect(),
            Stroke::new(3.2 * self.unit, color),
        ));
    }

    fn polygon(&self, points: &[[f32; 2]], color: Color32) {
        self.painter.add(Shape::convex_polygon(
            points.iter().map(|&p| self.at(p)).collect(),
            color,
            Stroke::NONE,
        ));
    }

    fn block(&self, a: [f32; 2], b: [f32; 2], color: Color32) {
        self.painter
            .rect_filled(Rect::from_min_max(self.at(a), self.at(b)), 2, color);
    }

    fn star(&self, x: f32, y: f32, scale: f32, color: Color32) {
        self.polygon(
            &[
                [x, y - scale],
                [x + scale * 0.34, y],
                [x, y + scale],
                [x - scale * 0.34, y],
            ],
            color,
        );
        self.polygon(
            &[
                [x - scale, y],
                [x, y - scale * 0.34],
                [x + scale, y],
                [x, y + scale * 0.34],
            ],
            color,
        );
    }

    fn draw(&self, motif: Motif) {
        use Motif::*;
        let (ink, soft, dark) = (self.ink, self.soft, self.dark);
        match motif {
            Wave | Water => {
                for offset in [-11.0, 0.0, 11.0] {
                    let points: Vec<_> = (0..=16)
                        .map(|step| {
                            let t = step as f32 / 16.0;
                            [
                                -23.0 + 46.0 * t,
                                offset + 6.0 * (t * std::f32::consts::TAU).sin(),
                            ]
                        })
                        .collect();
                    self.line(&points, if offset == 0.0 { ink } else { soft });
                }
            }
            Compass => {
                self.ring([0.0, 0.0], 21.0, soft);
                self.polygon(
                    &[[-13.0, 13.0], [-6.0, -6.0], [13.0, -13.0], [6.0, 6.0]],
                    ink,
                );
                self.disc([0.0, 0.0], 2.5, dark);
            }
            Heart => {
                self.disc([-10.0, -6.0], 12.0, ink);
                self.disc([10.0, -6.0], 12.0, ink);
                self.polygon(
                    &[
                        [-20.0, -1.0],
                        [20.0, -1.0],
                        [15.0, 8.0],
                        [0.0, 22.0],
                        [-15.0, 8.0],
                    ],
                    ink,
                );
                self.line(&[[-16.0, -6.0], [-13.0, -11.0], [-9.0, -12.0]], soft);
            }
            Flame => {
                self.disc([0.0, 7.0], 17.0, ink);
                self.polygon(
                    &[
                        [-16.0, 7.0],
                        [-12.0, -10.0],
                        [0.0, -25.0],
                        [14.0, -5.0],
                        [17.0, 7.0],
                    ],
                    ink,
                );
                self.disc([1.0, 11.0], 8.0, soft);
                self.polygon(&[[-7.0, 11.0], [2.0, -4.0], [9.0, 11.0]], soft);
            }
            Talk => {
                self.block([-23.0, -17.0], [23.0, 15.0], ink);
                self.polygon(&[[-16.0, 12.0], [-5.0, 12.0], [-16.0, 22.0]], ink);
                for x in [-10.0, 0.0, 10.0] {
                    self.disc([x, -1.0], 2.4, dark);
                }
            }
            Globe => {
                self.ring([0.0, 0.0], 21.0, ink);
                self.line(&[[-20.0, 0.0], [20.0, 0.0]], soft);
                self.line(
                    &[
                        [0.0, -20.0],
                        [-7.0, -11.0],
                        [-9.0, 0.0],
                        [-7.0, 11.0],
                        [0.0, 20.0],
                    ],
                    ink,
                );
                self.line(
                    &[
                        [0.0, -20.0],
                        [7.0, -11.0],
                        [9.0, 0.0],
                        [7.0, 11.0],
                        [0.0, 20.0],
                    ],
                    ink,
                );
            }
            Note => {
                self.disc([-10.0, 13.0], 8.0, ink);
                self.line(
                    &[
                        [-2.0, 13.0],
                        [-2.0, -20.0],
                        [17.0, -14.0],
                        [17.0, -8.0],
                        [-2.0, -14.0],
                    ],
                    ink,
                );
                self.star(13.0, 14.0, 6.0, soft);
            }
            Sun | Dawn => {
                if matches!(motif, Dawn) {
                    self.disc([0.0, 0.0], 12.0, ink);
                    self.block([-22.0, 4.0], [22.0, 14.0], dark);
                    self.line(&[[-23.0, 6.0], [23.0, 6.0]], soft);
                    self.line(&[[-15.0, 14.0], [15.0, 14.0]], ink);
                } else {
                    self.disc([0.0, 0.0], 12.0, ink);
                }
                for step in 0..if matches!(motif, Dawn) { 5 } else { 8 } {
                    let angle = std::f32::consts::PI + step as f32 * std::f32::consts::FRAC_PI_4;
                    let (y, x) = angle.sin_cos();
                    self.line(&[[x * 17.0, y * 17.0], [x * 22.0, y * 22.0]], soft);
                }
            }
            Drop => {
                self.disc([0.0, 7.0], 14.0, ink);
                self.polygon(&[[-12.0, 0.0], [0.0, -23.0], [12.0, 0.0]], ink);
                self.line(&[[-8.0, 4.0], [-8.0, 10.0], [-4.0, 14.0]], soft);
            }
            Bolt => {
                self.polygon(
                    &[[-5.0, -22.0], [15.0, -22.0], [0.0, -2.0], [-20.0, 4.0]],
                    ink,
                );
                self.polygon(
                    &[
                        [-20.0, 4.0],
                        [18.0, -5.0],
                        [15.0, 1.0],
                        [-7.0, 23.0],
                        [-4.0, 4.0],
                    ],
                    ink,
                );
                self.line(&[[-3.0, -16.0], [5.0, -16.0]], soft);
            }
            Stars => {
                self.star(-6.0, 0.0, 17.0, ink);
                self.star(14.0, -13.0, 7.0, soft);
                self.star(17.0, 11.0, 5.0, soft);
            }
            Moon => {
                let mut mesh = eframe::egui::Mesh::default();
                for step in 0..=16 {
                    let angle = std::f32::consts::PI * step as f32 / 16.0;
                    let (sin, cos) = angle.sin_cos();
                    let y = -21.0 * cos;
                    let tilt = y * 0.32;
                    mesh.colored_vertex(self.at([tilt - 20.0 * sin, y]), ink);
                    mesh.colored_vertex(self.at([tilt - 8.0 * sin, y]), ink);
                    if step > 0 {
                        let p = (step - 1) * 2;
                        mesh.add_triangle(p, p + 1, p + 2);
                        mesh.add_triangle(p + 1, p + 3, p + 2);
                    }
                }
                self.painter.add(Shape::mesh(mesh));
                self.star(12.0, -10.0, 7.0, soft);
            }
            Peak => {
                self.polygon(&[[-24.0, 15.0], [-7.0, -15.0], [7.0, 15.0]], soft);
                self.polygon(&[[-9.0, 15.0], [8.0, -22.0], [24.0, 15.0]], ink);
                self.polygon(
                    &[[1.0, -6.0], [8.0, -22.0], [15.0, -6.0], [8.0, -10.0]],
                    soft,
                );
            }
            Leaf => {
                self.polygon(
                    &[
                        [-18.0, 12.0],
                        [-17.0, 0.0],
                        [-9.0, -13.0],
                        [5.0, -19.0],
                        [22.0, -22.0],
                        [17.0, -4.0],
                        [9.0, 9.0],
                        [-4.0, 15.0],
                    ],
                    ink,
                );
                self.line(&[[-20.0, 20.0], [1.0, 0.0], [13.0, -11.0]], dark);
                self.line(&[[-4.0, 5.0], [-5.0, -6.0]], dark);
            }
            Book => {
                self.polygon(
                    &[
                        [-22.0, -15.0],
                        [-12.0, -15.0],
                        [-2.0, -10.0],
                        [-2.0, 17.0],
                        [-12.0, 12.0],
                        [-22.0, 12.0],
                    ],
                    soft,
                );
                self.polygon(
                    &[
                        [2.0, -10.0],
                        [12.0, -15.0],
                        [22.0, -15.0],
                        [22.0, 12.0],
                        [12.0, 12.0],
                        [2.0, 17.0],
                    ],
                    ink,
                );
                self.line(&[[-16.0, -7.0], [-11.0, -7.0], [-6.0, -4.0]], dark);
            }
            Desk => {
                self.block([-21.0, -18.0], [21.0, 10.0], ink);
                self.block([-16.0, -13.0], [16.0, 4.0], dark);
                self.line(&[[-9.0, 18.0], [9.0, 18.0]], soft);
                self.line(&[[0.0, 9.0], [0.0, 18.0]], soft);
                self.line(
                    &[
                        [-9.0, -4.0],
                        [-4.0, -4.0],
                        [-1.0, -8.0],
                        [4.0, 0.0],
                        [9.0, -4.0],
                    ],
                    soft,
                );
            }
            Run => {
                self.disc([8.0, -17.0], 5.0, soft);
                self.line(
                    &[[4.0, -11.0], [-3.0, 3.0], [-13.0, 9.0], [-20.0, 9.0]],
                    ink,
                );
                self.line(&[[-3.0, 3.0], [8.0, 7.0], [3.0, 20.0]], ink);
                self.line(
                    &[
                        [-11.0, -7.0],
                        [-4.0, -13.0],
                        [5.0, -10.0],
                        [11.0, -4.0],
                        [19.0, -7.0],
                    ],
                    soft,
                );
            }
            Weights => {
                self.block([-9.0, -3.0], [9.0, 3.0], soft);
                self.block([-17.0, -13.0], [-8.0, 13.0], ink);
                self.block([8.0, -13.0], [17.0, 13.0], ink);
                self.block([-23.0, -8.0], [-17.0, 8.0], soft);
                self.block([17.0, -8.0], [23.0, 8.0], soft);
            }
            Road => {
                self.polygon(
                    &[[-24.0, 17.0], [-8.0, -17.0], [8.0, -17.0], [24.0, 17.0]],
                    ink,
                );
                self.line(&[[0.0, -10.0], [0.0, -4.0]], dark);
                self.line(&[[0.0, 1.0], [0.0, 11.0]], dark);
                self.star(18.0, -15.0, 5.0, soft);
            }
            Route => {
                self.line(
                    &[
                        [-17.0, 14.0],
                        [-7.0, 7.0],
                        [-5.0, -4.0],
                        [4.0, -11.0],
                        [16.0, -11.0],
                    ],
                    ink,
                );
                self.disc([-17.0, 14.0], 5.0, soft);
                self.disc([16.0, -11.0], 6.0, ink);
                self.disc([16.0, -11.0], 2.5, dark);
            }
            Confetti => {
                self.polygon(&[[-21.0, 21.0], [-11.0, -10.0], [13.0, 9.0]], ink);
                self.line(&[[-15.0, 5.0], [-5.0, 15.0]], dark);
                self.star(14.0, -13.0, 8.0, soft);
                self.disc([22.0, 4.0], 3.0, ink);
                self.disc([-1.0, -19.0], 3.0, soft);
            }
            Guitar => {
                self.disc([-9.0, 9.0], 13.0, ink);
                self.disc([1.0, 0.0], 9.0, ink);
                self.line(&[[-6.0, 7.0], [16.0, -18.0], [19.0, -21.0]], soft);
                self.line(&[[-11.0, 5.0], [-5.0, 11.0]], dark);
            }
            Headphones => {
                self.line(
                    &[
                        [-19.0, 6.0],
                        [-18.0, -4.0],
                        [-12.0, -14.0],
                        [0.0, -18.0],
                        [12.0, -14.0],
                        [18.0, -4.0],
                        [19.0, 6.0],
                    ],
                    soft,
                );
                self.block([-22.0, 1.0], [-12.0, 17.0], ink);
                self.block([12.0, 1.0], [22.0, 17.0], ink);
            }
            Record | Disc => {
                self.disc([0.0, 0.0], 21.0, ink);
                self.ring([0.0, 0.0], 15.0, soft);
                self.disc(
                    [0.0, 0.0],
                    7.0,
                    if matches!(motif, Disc) { soft } else { dark },
                );
                self.disc(
                    [0.0, 0.0],
                    2.0,
                    if matches!(motif, Disc) { dark } else { soft },
                );
            }
            Sax => {
                self.line(
                    &[
                        [12.0, -17.0],
                        [3.0, -17.0],
                        [-2.0, -12.0],
                        [-2.0, 6.0],
                        [-7.0, 14.0],
                        [-14.0, 14.0],
                        [-19.0, 7.0],
                        [-19.0, -3.0],
                    ],
                    ink,
                );
                self.polygon(
                    &[[-24.0, -9.0], [-10.0, -9.0], [-16.0, 3.0], [-22.0, 3.0]],
                    ink,
                );
                for y in [-8.0, 0.0, 8.0] {
                    self.disc([4.0, y], 2.3, soft);
                }
            }
            Synth | Keys => {
                self.block([-23.0, -14.0], [23.0, 14.0], ink);
                if matches!(motif, Synth) {
                    self.line(
                        &[
                            [-17.0, -5.0],
                            [-10.0, -5.0],
                            [-10.0, -9.0],
                            [-3.0, -9.0],
                            [-3.0, -5.0],
                            [5.0, -5.0],
                            [5.0, -9.0],
                            [12.0, -9.0],
                            [12.0, -5.0],
                            [17.0, -5.0],
                        ],
                        dark,
                    );
                }
                let y = if matches!(motif, Synth) { 0.0 } else { -11.0 };
                for x in [-14.0, -5.0, 4.0, 13.0] {
                    self.line(&[[x, y], [x, 11.0]], dark);
                }
            }
            Mic => {
                self.disc([6.0, -9.0], 11.0, ink);
                self.polygon(
                    &[[-4.0, -4.0], [12.0, 1.0], [-14.0, 21.0], [-19.0, 15.0]],
                    soft,
                );
                self.line(&[[-1.0, -14.0], [11.0, -8.0]], dark);
            }
            Film => {
                self.block([-22.0, -13.0], [22.0, 15.0], ink);
                self.block([-21.0, -21.0], [22.0, -15.0], soft);
                self.polygon(&[[-5.0, -7.0], [8.0, 1.0], [-5.0, 9.0]], dark);
            }
            Cassette => {
                self.block([-23.0, -14.0], [23.0, 14.0], ink);
                self.block([-15.0, -7.0], [15.0, 4.0], dark);
                self.disc([-9.0, -1.5], 3.0, soft);
                self.disc([9.0, -1.5], 3.0, soft);
                self.polygon(
                    &[[-12.0, 13.0], [-8.0, 7.0], [8.0, 7.0], [12.0, 13.0]],
                    soft,
                );
            }
            Player => {
                self.block([-13.0, -22.0], [13.0, 22.0], ink);
                self.block([-9.0, -17.0], [9.0, -3.0], dark);
                self.disc([0.0, 10.0], 8.0, soft);
                self.disc([0.0, 10.0], 3.0, dark);
            }
        }
    }
}

pub(crate) fn paint(painter: &Painter, rect: Rect, id: Option<&str>, opacity: f32) {
    let (motif, [r, g, b]) = style(id);
    let tint = |color: Color32| color.gamma_multiply(opacity.clamp(0.0, 1.0));
    Art {
        painter,
        origin: pos2(rect.center().x, rect.center().y),
        unit: rect.width().min(rect.height()) / 54.0,
        ink: tint(Color32::from_rgb(r, g, b)),
        soft: tint(Color32::from_rgb(
            r.saturating_add(28),
            g.saturating_add(32),
            b.saturating_add(34),
        )),
        dark: tint(Color32::from_rgb(29, 30, 41)),
    }
    .draw(motif);
}
