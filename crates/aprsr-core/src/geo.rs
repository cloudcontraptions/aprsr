//! Geographic helpers shared by the range-based filters.

/// Mean Earth radius in kilometres (IUGG mean radius R₁).
pub const EARTH_RADIUS_KM: f64 = 6371.0088;

/// Great-circle distance in kilometres between two WGS-84 coordinates.
///
/// Uses the haversine formula, which stays numerically well conditioned at the small
/// separations that dominate APRS range filters — the spherical law of cosines loses
/// precision below a kilometre or so.
///
/// Inputs are degrees; latitude is positive north, longitude positive east.
#[must_use]
pub fn great_circle_distance_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let (phi1, phi2) = (lat1.to_radians(), lat2.to_radians());
    let d_phi = phi2 - phi1;
    let d_lambda = (lon2 - lon1).to_radians();

    let a = (d_phi / 2.0).sin().powi(2) + phi1.cos() * phi2.cos() * (d_lambda / 2.0).sin().powi(2);

    2.0 * EARTH_RADIUS_KM * a.sqrt().clamp(-1.0, 1.0).asin()
}

/// True when `(lat, lon)` falls inside the box described by an APRS-IS `a/` filter.
///
/// Per <http://www.aprs-is.net/javAPRSFilter.aspx> the argument order is
/// `a/latNorth/lonWest/latSouth/lonEast` — the north-west corner comes first. The
/// comparison is inclusive on all four edges.
///
/// Boxes that cross the antimeridian (`lon_west > lon_east`) are handled by treating the
/// longitude span as wrapping.
#[must_use]
pub fn within_area(
    lat: f64,
    lon: f64,
    lat_north: f64,
    lon_west: f64,
    lat_south: f64,
    lon_east: f64,
) -> bool {
    if lat > lat_north || lat < lat_south {
        return false;
    }
    if lon_west <= lon_east {
        lon >= lon_west && lon <= lon_east
    } else {
        // Wrapped box, e.g. a/10/170/-10/-170 spanning the antimeridian.
        lon >= lon_west || lon <= lon_east
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Distances are asserted against independently computed haversine values.
    #[test]
    fn distance_matches_known_pairs() {
        // Helsinki to Tampere.
        let d = great_circle_distance_km(60.1699, 24.9384, 61.4978, 23.7610);
        assert!((d - 160.845).abs() < 0.01, "got {d}");

        // Dallas to Oklahoma City.
        let d = great_circle_distance_km(32.7767, -96.7970, 35.4676, -97.5164);
        assert!((d - 306.452).abs() < 0.01, "got {d}");
    }

    #[test]
    fn distance_to_self_is_zero() {
        assert!(great_circle_distance_km(60.0, 25.0, 60.0, 25.0).abs() < f64::EPSILON);
    }

    #[test]
    fn distance_is_symmetric() {
        let a = great_circle_distance_km(60.0, 25.0, 35.0, -97.0);
        let b = great_circle_distance_km(35.0, -97.0, 60.0, 25.0);
        assert!((a - b).abs() < 1e-9);
    }

    /// Antipodal points are half the circumference apart. This is where the law of
    /// cosines would fall apart; haversine handles it.
    #[test]
    fn antipodal_distance_is_half_circumference() {
        let d = great_circle_distance_km(0.0, 0.0, 0.0, 180.0);
        let expected = std::f64::consts::PI * EARTH_RADIUS_KM;
        assert!((d - expected).abs() < 1.0, "got {d}, want {expected}");
    }

    #[test]
    fn area_box_includes_interior_and_edges() {
        // a/40/-100/30/-90 — a box over the central United States.
        assert!(within_area(35.0, -95.0, 40.0, -100.0, 30.0, -90.0));
        assert!(
            within_area(40.0, -100.0, 40.0, -100.0, 30.0, -90.0),
            "NW corner"
        );
        assert!(
            within_area(30.0, -90.0, 40.0, -100.0, 30.0, -90.0),
            "SE corner"
        );
    }

    #[test]
    fn area_box_excludes_outside() {
        assert!(
            !within_area(45.0, -95.0, 40.0, -100.0, 30.0, -90.0),
            "too far north"
        );
        assert!(
            !within_area(25.0, -95.0, 40.0, -100.0, 30.0, -90.0),
            "too far south"
        );
        assert!(
            !within_area(35.0, -105.0, 40.0, -100.0, 30.0, -90.0),
            "too far west"
        );
        assert!(
            !within_area(35.0, -85.0, 40.0, -100.0, 30.0, -90.0),
            "too far east"
        );
    }

    #[test]
    fn area_box_wraps_antimeridian() {
        // a/10/170/-10/-170 — a box straddling 180°.
        assert!(within_area(0.0, 179.0, 10.0, 170.0, -10.0, -170.0));
        assert!(within_area(0.0, -179.0, 10.0, 170.0, -10.0, -170.0));
        assert!(!within_area(0.0, 0.0, 10.0, 170.0, -10.0, -170.0));
    }
}
