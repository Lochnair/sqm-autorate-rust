use std::error::Error;
use std::net::IpAddr;

pub trait Pinger {
    fn new(id: u16, reflectors: Vec<IpAddr>) -> Self;
    fn receive_loop(&mut self);
    fn sender_loop(&mut self);
    fn send_ping(&mut self, reflector: &IpAddr, id: u16, seq: u16) -> Result<(), Box<dyn Error>>;
}
