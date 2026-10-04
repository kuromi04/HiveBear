use std::net::UdpSocket;
fn main() {
    let socket = UdpSocket::bind("0.0.0.0:0").unwrap();
    socket.connect("8.8.8.8:80").unwrap();
    println!("{}", socket.local_addr().unwrap().ip());
}
