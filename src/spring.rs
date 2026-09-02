//! niri's closed-form spring, ported from mosaic (`mosaic/crates/app/src/spring.rs`),
//! which transcribed it from niri. Evaluated at absolute elapsed time, so a
//! dropped frame cannot perturb the trajectory; retargeting preserves position
//! *and* velocity, so chained motions keep their momentum.

/// Mass is fixed at 1.0, as in niri.
const MASS: f64 = 1.0;

/// Spring coefficients.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpringParams {
    stiffness: f64,
    damping: f64,
    epsilon: f64,
}

impl SpringParams {
    /// From stiffness, damping *ratio* (1.0 = critically damped) and epsilon.
    #[must_use]
    pub fn new(stiffness: f64, damping_ratio: f64, epsilon: f64) -> Self {
        let critical = 2.0 * (MASS * stiffness).sqrt();
        Self {
            stiffness,
            damping: damping_ratio * critical,
            epsilon,
        }
    }

    /// niri's window-movement spring: `k = 800, ζ = 1, ε = 1e-4` (~326 ms).
    /// Used for panel rects and the camera.
    #[must_use]
    pub fn movement() -> Self {
        Self::new(800.0, 1.0, 1e-4)
    }

    /// A snappier spring for small fades (open/close alpha).
    #[must_use]
    pub fn fade() -> Self {
        Self::new(1600.0, 1.0, 1e-4)
    }

    /// The overlay chassis' presence (`k = 1200, ζ = 1`, ~200 ms): quicker
    /// than a panel's movement, so a palette feels summoned rather than
    /// animated, and slow enough that its rise reads as motion.
    #[must_use]
    pub fn overlay() -> Self {
        Self::new(1200.0, 1.0, 1e-3)
    }

    fn beta(&self) -> f64 {
        self.damping / (2.0 * MASS)
    }

    /// Total duration: `-ln(ε) / β` for `ζ <= 1` (all springs we configure).
    fn duration(&self) -> f64 {
        let beta = self.beta();
        if beta <= 0.0 || !self.epsilon.is_finite() || self.epsilon <= 0.0 {
            return 0.0;
        }
        (-self.epsilon.ln() / beta).max(0.0)
    }
}

/// A one-dimensional spring towards a target.
#[derive(Debug, Clone, Copy)]
pub struct Spring {
    from: f64,
    to: f64,
    v0: f64,
    params: SpringParams,
    t: f64,
    duration: f64,
}

impl Spring {
    /// A spring at rest at `value`.
    #[must_use]
    pub fn at_rest(value: f64, params: SpringParams) -> Self {
        Self {
            from: value,
            to: value,
            v0: 0.0,
            params,
            t: 0.0,
            duration: 0.0,
        }
    }

    /// The current target.
    #[must_use]
    pub fn target(&self) -> f64 {
        self.to
    }

    /// True once the animation has run its full duration (time-based, as niri).
    #[must_use]
    pub fn is_done(&self) -> bool {
        self.t >= self.duration
    }

    /// Current position; snaps exactly to the target once done.
    #[must_use]
    pub fn value(&self) -> f64 {
        if self.is_done() {
            return self.to;
        }
        self.clamp_value(self.oscillate(self.t))
    }

    /// Current velocity in units/second; zero once done.
    #[must_use]
    pub fn velocity(&self) -> f64 {
        if self.is_done() {
            return 0.0;
        }
        self.oscillate_velocity(self.t)
    }

    /// Advances the clock by `dt` seconds. The `min(1.0)` is a numerical guard,
    /// not policy — the frame clock decides what `dt` is allowed to be.
    pub fn advance(&mut self, dt: f64) {
        if !dt.is_finite() || dt <= 0.0 {
            return;
        }
        self.t = (self.t + dt.min(1.0)).min(self.duration);
    }

    /// Points the spring at a new target, preserving position and velocity.
    /// Retargeting a resting spring to its own value is a no-op.
    pub fn retarget(&mut self, to: f64) {
        if !to.is_finite() || self.to == to {
            return;
        }
        let value = self.value();
        let velocity = self.velocity();
        self.from = value;
        self.v0 = velocity;
        self.to = to;
        self.t = 0.0;
        self.duration = self.params.duration();
    }

    /// Teleports to `value` with zero velocity and no animation.
    pub fn jump_to(&mut self, value: f64) {
        self.from = value;
        self.to = value;
        self.v0 = 0.0;
        self.t = 0.0;
        self.duration = 0.0;
    }

    /// niri clamps output to ±10× the from→to range for numerical stability.
    fn clamp_value(&self, v: f64) -> f64 {
        let range = (self.to - self.from).abs();
        if range == 0.0 {
            return v;
        }
        let slack = range * 10.0;
        v.clamp(self.to - slack, self.to + slack)
    }

    fn oscillate(&self, t: f64) -> f64 {
        let beta = self.params.beta();
        let omega0 = (self.params.stiffness / MASS).sqrt();
        let x0 = self.from - self.to;
        let v0 = self.v0;
        let envelope = (-beta * t).exp();
        if (beta - omega0).abs() <= f64::from(f32::EPSILON) {
            // Critically damped.
            self.to + envelope * (x0 + (beta * x0 + v0) * t)
        } else if beta < omega0 {
            // Underdamped.
            let omega1 = (omega0 * omega0 - beta * beta).sqrt();
            self.to
                + envelope
                    * (x0 * (omega1 * t).cos() + ((beta * x0 + v0) / omega1) * (omega1 * t).sin())
        } else {
            // Overdamped.
            let omega2 = (beta * beta - omega0 * omega0).sqrt();
            self.to
                + envelope
                    * (x0 * (omega2 * t).cosh() + ((beta * x0 + v0) / omega2) * (omega2 * t).sinh())
        }
    }

    fn oscillate_velocity(&self, t: f64) -> f64 {
        let beta = self.params.beta();
        let omega0 = (self.params.stiffness / MASS).sqrt();
        let x0 = self.from - self.to;
        let v0 = self.v0;
        let envelope = (-beta * t).exp();
        if (beta - omega0).abs() <= f64::from(f32::EPSILON) {
            let c = beta * x0 + v0;
            envelope * (c * (1.0 - beta * t) - beta * x0)
        } else if beta < omega0 {
            let omega1 = (omega0 * omega0 - beta * beta).sqrt();
            let a = x0;
            let c = (beta * x0 + v0) / omega1;
            envelope
                * ((c * omega1 - beta * a) * (omega1 * t).cos()
                    - (a * omega1 + beta * c) * (omega1 * t).sin())
        } else {
            let omega2 = (beta * beta - omega0 * omega0).sqrt();
            let a = x0;
            let c = (beta * x0 + v0) / omega2;
            envelope
                * ((c * omega2 - beta * a) * (omega2 * t).cosh()
                    + (a * omega2 - beta * c) * (omega2 * t).sinh())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settles_on_target() {
        let mut s = Spring::at_rest(0.0, SpringParams::movement());
        s.retarget(100.0);
        for _ in 0..120 {
            s.advance(1.0 / 120.0);
        }
        assert!(s.is_done());
        assert_eq!(s.value(), 100.0);
    }

    #[test]
    fn retarget_preserves_motion() {
        let mut s = Spring::at_rest(0.0, SpringParams::movement());
        s.retarget(100.0);
        for _ in 0..6 {
            s.advance(1.0 / 120.0);
        }
        let v = s.velocity();
        assert!(v > 0.0);
        s.retarget(200.0);
        assert!((s.velocity() - v).abs() < 1e-9);
        assert!(!s.is_done());
    }

    #[test]
    fn resting_retarget_to_same_value_is_noop() {
        let mut s = Spring::at_rest(5.0, SpringParams::movement());
        s.retarget(5.0);
        assert!(s.is_done());
        assert_eq!(s.value(), 5.0);
    }
}
