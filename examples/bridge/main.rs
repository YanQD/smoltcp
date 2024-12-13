extern crate alloc;

pub mod utils;
pub mod switch;
pub mod frame;

use spin::Mutex;
use switch::Switch;
use frame::FrameCapture;

use std::thread;
use std::sync::Arc;

use smoltcp::socket::udp;
use smoltcp::time::Instant;
use smoltcp::iface::{Config, Interface, SocketSet};
use smoltcp::phy::Medium;
use smoltcp::wire::{EthernetAddress, HardwareAddress, IpAddress, IpCidr, Ipv4Address, Ipv6Address};

pub const BRIDGE_MAC: [u8; 6] = [0x02, 0x00, 0x00, 0x00, 0x00, 0x00];
// phy::TunTapInterface: tap0
pub const PORT1_MAC: [u8; 6] = [0x02, 0x00, 0x00, 0x00, 0x00, 0x02];
// phy::TunTapInterface: tap1
pub const PORT2_MAC: [u8; 6] = [0x02, 0x00, 0x00, 0x00, 0x00, 0x03];
// phy::TunTapInterface: tap2
pub const PORT3_MAC: [u8; 6] = [0x02, 0x00, 0x00, 0x00, 0x00, 0x04];
// Bridge Max Ports
pub const MAX_PORTS: u8 = 8;

pub fn get_bridge_mac() -> EthernetAddress {
    EthernetAddress::from_bytes(&BRIDGE_MAC)
}

pub fn get_port1_mac() -> EthernetAddress {
    EthernetAddress::from_bytes(&PORT1_MAC)
}

pub fn get_port2_mac() -> EthernetAddress {
    EthernetAddress::from_bytes(&PORT2_MAC)
}

pub fn get_port3_mac() -> EthernetAddress {
    EthernetAddress::from_bytes(&PORT3_MAC)
}

fn main() {
    utils::setup_logging("");

    // 创建两个TAP设备作为测试端口
    // let device0 = FrameCapture::new("tap2", Medium::Ethernet).unwrap();
    let mut device1 = FrameCapture::new("tap1", Medium::Ethernet).unwrap();
    let mut device2 = FrameCapture::new("tap0", Medium::Ethernet).unwrap();

    // let config0 = Config::new(HardwareAddress::Ethernet(get_port1_mac()));
    let config1 = Config::new(HardwareAddress::Ethernet(get_port2_mac()));
    let config2 = Config::new(HardwareAddress::Ethernet(get_port3_mac()));

    let mut iface1 = Interface::new(config1.clone(), &mut device1, Instant::now());
    iface1.update_ip_addrs(|ip_addrs| {
        ip_addrs
            .push(IpCidr::new(IpAddress::v4(192, 168, 69, 6), 24))
            .unwrap();
        ip_addrs
            .push(IpCidr::new(IpAddress::v6(0xfdaa, 0, 0, 0, 0, 0, 0, 1), 64))
            .unwrap();
    });
    iface1
        .routes_mut()
        .add_default_ipv4_route(Ipv4Address::new(192, 168, 69, 100))
        .unwrap();
    iface1
        .routes_mut()
        .add_default_ipv6_route(Ipv6Address::new(0xfe80, 0, 0, 0, 0, 0, 0, 0x100))
        .unwrap();

    let mut iface2 = Interface::new(config2.clone(), &mut device2, Instant::now());
    iface2.update_ip_addrs(|ip_addrs| {
        ip_addrs
            .push(IpCidr::new(IpAddress::v4(192, 168, 69, 7), 24))
            .unwrap();
        ip_addrs
            .push(IpCidr::new(IpAddress::v6(0xfdaa, 0, 0, 0, 0, 0, 0, 2), 64))
            .unwrap();
    });
    iface2
        .routes_mut()
        .add_default_ipv4_route(Ipv4Address::new(192, 168, 69, 100))
        .unwrap();
    iface2
        .routes_mut()
        .add_default_ipv6_route(Ipv6Address::new(0xfe80, 0, 0, 0, 0, 0, 0, 0x101))
        .unwrap();

    // 创建网桥实例
    let switch = Arc::new(Mutex::new(Switch::new(
        EthernetAddress([0x02, 0x00, 0x00, 0x00, 0x00, 0x04]),
        2,  // max_ports
    )));

    // 添加端口到网桥
    switch.lock().add_port(
        iface1,
        device1,
        0, 
    ).unwrap();

    switch.lock().add_port(
        iface2,
        device2,
        1,
    ).unwrap();

    let mut sockets1 = SocketSet::new(vec![]);
    let mut sockets2 = SocketSet::new(vec![]);

    let udp_rx_buffer1 = udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 64], vec![0; 65535]);
    let udp_tx_buffer1 = udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 64], vec![0; 65535]);
    let udp_rx_buffer2 = udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 64], vec![0; 65535]);
    let udp_tx_buffer2 = udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 64], vec![0; 65535]);
    let udp_socket1 = udp::Socket::new(udp_rx_buffer1, udp_tx_buffer1);
    let udp_socket2 = udp::Socket::new(udp_rx_buffer2, udp_tx_buffer2);

    let udp_handle1 = sockets1.add(udp_socket1);
    let udp_handle2 = sockets2.add(udp_socket2);

    // 创建客户端线程
    let bridge_clone1 = switch.clone();
    let server_thread = thread::spawn(move || {
        println!("\x1b[31mThis is in Sender\x1b[0m");

        // 处理发送
        loop {
            println!("\x1b[35mClient loop\x1b[0m");
            let timestamp1 = Instant::now();

            // bridge poll
            println!("\x1b[35mClient Second: Bridge poll\x1b[0m");
            let mut bridge_guard = bridge_clone1.lock();
            bridge_guard.poll(
                timestamp1, 
                &mut sockets1,
                udp_handle1,
                0,
                |sockets1| {
                    let socket1 = sockets1.get_mut::<udp::Socket>(udp_handle1);
                    if !socket1.is_open() {
                        socket1.bind(7969).unwrap();
                        println!("\x1b[34mSender socket bound to port 7969\x1b[0m");
                    }
        
                    let data: &[u8; 18] = b"Hello from sender!";
                    match socket1.send_slice(data, (IpAddress::v4(192, 168, 69, 4), 7979)) {
                        Ok(_) => println!("\x1b[35mClient First: Sent data {:?} to {}:{}\x1b[0m", data, IpAddress::v4(192, 168, 69, 4), 7979),
                        Err(e) => println!("Failed to send data: {}", e),
                    }
                }
            );
            drop(bridge_guard);
        }
    });

    // // 创建服务端线程
    let bridge_clone2 = switch.clone();
    let client_thread = thread::spawn(move || {
        println!("\x1b[31mThis is in Receiver\x1b[0m");
        
        // 处理接收以及发送
        loop {
            println!("\x1b[35mServer loop\x1b[0m");
            let timestamp2 = Instant::now();

            // bridge poll
            println!("\x1b[35mServer Second: Bridge poll\x1b[0m");
            let mut bridge_guard = bridge_clone2.lock();
            bridge_guard.poll(
                timestamp2,
                &mut sockets2,
                udp_handle2,
                1, 
                |sockets| {
                    let socket = sockets.get_mut::<udp::Socket>(udp_handle2);
                    if !socket.is_open() {
                        socket.bind(7979).unwrap();
                        println!("\x1b[35mReceiver socket bound to port 7979\x1b[0m");
                    }
            
                    let server = match socket.recv() {
                        Ok((data, endpoint)) => {
                            println!("udp: recv data: {:?} from {}", data, endpoint);
                            let mut data = data.to_vec();
                            data.reverse();
                            Some((endpoint, data))
                        }
                        Err(_) => None,
                    };
            
                    if let Some((endpoint, data)) = server {
                        println!("udp: send data: {:?} to {}", data, endpoint);
                        socket.send_slice(&data, endpoint).unwrap();
                    }
                }
            );
            drop(bridge_guard);
        }
    });

    server_thread.join().unwrap();
    client_thread.join().unwrap();
}