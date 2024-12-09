extern crate alloc;

pub mod utils;
pub mod bridge;
pub mod bridge_fdb;

use log::debug;

use bridge::BridgeWrapper;

use std::thread;
use std::sync::Arc;
// use std::time::Duration;
use std::os::unix::io::AsRawFd;

use smoltcp::socket::udp;
use smoltcp::time::Instant;
use smoltcp::iface::{Config, Interface, SocketSet};
use smoltcp::phy::{wait as phy_wait, Medium, TunTapInterface};
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
    let device0 = TunTapInterface::new("tap2", Medium::Ethernet).unwrap();
    let _fd0 = device0.as_raw_fd();
    let mut device1 = TunTapInterface::new("tap1", Medium::Ethernet).unwrap();
    let fd1 = device1.as_raw_fd();
    let mut device2 = TunTapInterface::new("tap0", Medium::Ethernet).unwrap();
    let fd2 = device2.as_raw_fd();

    let config0 = Config::new(HardwareAddress::Ethernet(get_port1_mac()));
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
    let bridge = Arc::new(BridgeWrapper::new(
        config0,
        device0,
        EthernetAddress([0x02, 0x00, 0x00, 0x00, 0x00, 0x04]),
        2,  // max_ports
        Instant::now(),
    ));

    // 添加端口到网桥
    bridge.add_port(
        config1,
        device1,
        0, 
        Instant::now(),
    ).unwrap();

    bridge.add_port(
        config2,
        device2,
        1,
        Instant::now(),
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
    let bridge_clone1 = bridge.clone();
    let server_thread = thread::spawn(move || {
        println!("\x1b[31mThis is in Sender\x1b[0m");
        let bridgeport1 = bridge_clone1.get_bridgeport(0).unwrap();

        // 处理发送
        loop {
            let timestamp1 = Instant::now();
            
            bridgeport1.with_device_mut(|device| {
                iface1.poll(timestamp1, device, &mut sockets1);
            });

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

            // bridge poll
            println!("\x1b[35mClient Second: Bridge poll\x1b[0m");
            bridge_clone1.poll();
            
            if let Err(e) = phy_wait(fd1, iface1.poll_delay(timestamp1, &sockets1)) {
                println!("Sender wait error: {}", e);
            }
        }
    });

    // 创建服务端线程
    let bridge_clone2 = bridge.clone();
    let client_thread = thread::spawn(move || {
        println!("\x1b[31mThis is in Receiver\x1b[0m");
        let bridgeport2 = bridge_clone2.get_bridgeport(1).unwrap();
        
        // 处理接收以及发送
        loop {
            let timestamp2 = Instant::now();
            bridgeport2.with_device_mut(|device| {
                iface2.poll(timestamp2, device, &mut sockets2);
            });

            let socket2 = sockets2.get_mut::<udp::Socket>(udp_handle2);
            if !socket2.is_open() {
                socket2.bind(7979).unwrap();
                println!("\x1b[35mReceiver socket bound to port 7979\x1b[0m");
            }

            let server = match socket2.recv() {
                Ok((data, endpoint)) => {
                    println!("udp: recv data: {:?} from {}", data, endpoint);
                    let mut data = data.to_vec();
                    data.reverse();
                    Some((endpoint, data))
                }
                Err(_) => None,
            };

            if let Some((endpoint, data)) = server {
                println!("udp: send data: {:?} to {}", data, endpoint,);
                socket2.send_slice(&data, endpoint).unwrap();
            }

            // bridge poll
            bridge_clone2.poll();

            if let Err(e) = phy_wait(fd2, iface2.poll_delay(timestamp2, &sockets2)) {
                println!("Receiver wait error: {}", e);
            }
        }
    });

    server_thread.join().unwrap();
    client_thread.join().unwrap();
}