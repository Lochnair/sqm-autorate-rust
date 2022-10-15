pub struct Utils {}

impl Utils {
    pub fn a_else_b(a: f64, b: f64) -> f64 {
        if a > 0.0 {
            a
        } else {
            b
        }
    }
    pub fn ewma_factor(tick: f64, dur: f64) -> f64 {
        ((0.5_f64).ln() / (dur / tick)).exp()
    }
}
