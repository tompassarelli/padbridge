use padbridge::Sample;

#[cfg(target_os = "linux")]
pub struct AnalogSession {
    pub ingress: padbridge::pad_ingress::Ingress,
    pub out: padbridge::service::any_map::DesktopOutput,
}

#[cfg(target_os = "linux")]
impl AnalogSession {
    pub fn step(&mut self, sample: &Sample, eligible: bool) -> Result<(), String> {
        self.ingress.update(sample, eligible, &mut self.out)
    }
}

#[cfg(target_os = "linux")]
impl Drop for AnalogSession {
    fn drop(&mut self) { let _ = self.ingress.release(&mut self.out); }
}
