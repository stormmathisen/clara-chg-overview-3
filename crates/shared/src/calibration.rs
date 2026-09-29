//! Calibration-sweep fits, shared so the server logs the same numbers the UI shows and
//! the UI can switch fit models without re-sweeping.
//!
//! The IOC computes `Q = corrA · amplitude − corrB` (corrB is subtracted: zeroing a WCM
//! writes the mean zero-beam reading into corrB). Two models are fitted over the levels
//! used:
//!
//! * **through the origin** (`corrB = 0`): right where the chain compresses at high charge
//!   (FB3/FB4), since a free offset there would soak up the curvature and leave several pC
//!   at zero beam;
//! * **with an offset**: right at FB0/FB1, where small pulses fall short of proportional
//!   (FED, 2026-09-29: ~0.5 pC at FB0; RMS error 0.23 → 0.03 pC).

use crate::messages::CalibrationLevel;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fit {
    /// pC per ADC count (negative: pulses are negative-going).
    pub corr_a: f64,
    /// pC, subtracted by the IOC.
    pub corr_b: f64,
    /// RMS charge residual over the fitted levels (pC).
    pub rms_pc: f64,
}

/// (amplitude, nominal charge) of the levels used in the fit.
fn points(levels: &[CalibrationLevel]) -> Vec<(f64, f64)> {
    levels
        .iter()
        .filter(|l| l.used)
        .map(|l| (l.mean, l.charge))
        .collect()
}

fn with_rms(points: &[(f64, f64)], corr_a: f64, corr_b: f64) -> Fit {
    let ss: f64 = points
        .iter()
        .map(|(a, q)| (corr_a * a - corr_b - q).powi(2))
        .sum();
    Fit {
        corr_a,
        corr_b,
        rms_pc: (ss / points.len() as f64).sqrt(),
    }
}

/// Least squares of `Q = corrA · A` (corrB = 0). None without data or for a
/// positive-going result.
pub fn fit_origin(levels: &[CalibrationLevel]) -> Option<Fit> {
    let p = points(levels);
    let aa: f64 = p.iter().map(|(a, _)| a * a).sum();
    let qa: f64 = p.iter().map(|(a, q)| a * q).sum();
    let corr_a = (aa > 0.0).then(|| qa / aa).filter(|c| *c < 0.0)?;
    Some(with_rms(&p, corr_a, 0.0))
}

/// Least squares of `Q = corrA · A − corrB`. Needs at least 3 levels.
pub fn fit_offset(levels: &[CalibrationLevel]) -> Option<Fit> {
    let p = points(levels);
    if p.len() < 3 {
        return None;
    }
    let n = p.len() as f64;
    let ma = p.iter().map(|x| x.0).sum::<f64>() / n;
    let mq = p.iter().map(|x| x.1).sum::<f64>() / n;
    let saa: f64 = p.iter().map(|(a, _)| (a - ma).powi(2)).sum();
    let saq: f64 = p.iter().map(|(a, q)| (a - ma) * (q - mq)).sum();
    let corr_a = (saa > 0.0).then(|| saq / saa).filter(|c| *c < 0.0)?;
    Some(with_rms(&p, corr_a, corr_a * ma - mq))
}

/// Default model choice: the offset fit only when it at least halves the RMS error, i.e.
/// when the data really has an offset rather than curvature.
pub fn prefer_offset(origin: Option<Fit>, offset: Option<Fit>) -> bool {
    match (origin, offset) {
        (Some(o), Some(f)) => f.rms_pc * 2.0 <= o.rms_pc,
        (None, Some(_)) => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn levels(f: impl Fn(f64) -> f64) -> Vec<CalibrationLevel> {
        (1..=20)
            .map(|i| {
                let charge = i as f64;
                CalibrationLevel {
                    level: i as u8,
                    charge,
                    mean: f(charge),
                    sigma: 0.0,
                    used: true,
                }
            })
            .collect()
    }

    #[test]
    fn origin_fit_recovers_proportional_data() {
        let l = levels(|q| q / -0.00838);
        let f = fit_origin(&l).unwrap();
        assert!((f.corr_a + 0.00838).abs() < 1e-12);
        assert!(f.rms_pc < 1e-9);
        assert!(!prefer_offset(Some(f), fit_offset(&l)));
    }

    #[test]
    fn offset_fit_recovers_subtracted_corr_b() {
        // Q = corrA·A − corrB with corrB = −0.5 pC (reads 0.5 pC high at zero amplitude).
        let (corr_a, corr_b) = (-1.5e-4, -0.5);
        let l = levels(|q| (q + corr_b) / corr_a);
        let f = fit_offset(&l).unwrap();
        assert!((f.corr_a - corr_a).abs() < 1e-12, "{f:?}");
        assert!((f.corr_b - corr_b).abs() < 1e-9, "{f:?}");
        assert!(prefer_offset(fit_origin(&l), Some(f)));
    }

    #[test]
    fn unused_and_wrong_polarity_levels_are_rejected() {
        let mut l = levels(|q| q * 1000.0);
        assert!(fit_origin(&l).is_none() && fit_offset(&l).is_none());
        l.iter_mut().for_each(|x| x.used = false);
        assert!(fit_origin(&l).is_none() && fit_offset(&l).is_none());
    }
}
