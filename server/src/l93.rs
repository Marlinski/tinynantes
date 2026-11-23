//! WGS84 / RGF93 longitude-latitude -> Lambert-93 (EPSG:2154), Lambert conformal conic on GRS80.
//! RGF93 and WGS84 differ by well under a metre, below what the map can show.

use std::f64::consts::FRAC_PI_4;

const A: f64 = 6_378_137.0;
const F: f64 = 1.0 / 298.257_222_101;
const LON0: f64 = 3.0;
const LAT0: f64 = 46.5;
const LAT1: f64 = 49.0;
const LAT2: f64 = 44.0;
const X0: f64 = 700_000.0;
const Y0: f64 = 6_600_000.0;

fn e() -> f64 {
    (2.0 * F - F * F).sqrt()
}
fn m(phi: f64) -> f64 {
    let e = e();
    phi.cos() / (1.0 - e * e * phi.sin().powi(2)).sqrt()
}
fn t(phi: f64) -> f64 {
    let e = e();
    let s = e * phi.sin();
    (FRAC_PI_4 - phi / 2.0).tan() / ((1.0 - s) / (1.0 + s)).powf(e / 2.0)
}

pub fn project(lon: f64, lat: f64) -> (f64, f64) {
    let (p0, p1, p2) = (LAT0.to_radians(), LAT1.to_radians(), LAT2.to_radians());
    let n = (m(p1).ln() - m(p2).ln()) / (t(p1).ln() - t(p2).ln());
    let ff = m(p1) / (n * t(p1).powf(n));
    let rho = |phi: f64| A * ff * t(phi).powf(n);
    let th = n * (lon - LON0).to_radians();
    let r = rho(lat.to_radians());
    (X0 + r * th.sin(), Y0 + rho(p0) - r * th.cos())
}

#[cfg(test)]
mod tests {
    #[test]
    fn chateau() {
        // Château des ducs de Bretagne, checked against pyproj
        let (x, y) = super::project(-1.549_747, 47.216_027);
        assert!((x - 355_853.73).abs() < 0.05 && (y - 6_689_443.15).abs() < 0.05, "{x} {y}");
    }
}
