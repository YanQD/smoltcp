mod utils;

use log::debug;

use std::os::unix::io::AsRawFd;
use std::thread;
use std::time::Duration;

use smoltcp::iface::{Config, Interface, SocketSet};
use smoltcp::phy::{wait as phy_wait, Medium, TunTapInterface};
use smoltcp::socket::udp;
use smoltcp::time::Instant;
use smoltcp::wire::{EthernetAddress, IpAddress, IpCidr};

fn create_network_stack(name: &str, ip: [u8; 4], hw_addr: EthernetAddress) -> (Interface, TunTapInterface) {
    let mut device = TunTapInterface::new(name, Medium::Ethernet).unwrap();
    
    let mut config: Config  = Config::new(hw_addr.into()).into();
    config.random_seed = rand::random();

    let mut iface = Interface::new(config, &mut device, Instant::now());
    iface.update_ip_addrs(|ip_addrs| {
        ip_addrs
            .push(IpCidr::new(IpAddress::v4(ip[0], ip[1], ip[2], ip[3]), 24))
            .unwrap();
    });

    (iface, device)
}

fn main() {
    utils::setup_logging("");

    // 创建服务端线程
    let server_thread = thread::spawn(move || {
        println!("\x1b[31mServer thread started\x1b[0m");
        let (mut server_iface, mut server_device) = create_network_stack(
            "tap1", 
            [192, 168, 69, 1], 
            EthernetAddress([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]));
        let server_fd = server_device.as_raw_fd();

        // 创建服务端socket
        let server_socket = {
            let rx_buffer = udp::PacketBuffer::new(
                vec![udp::PacketMetadata::EMPTY; 8],
                vec![0; 65535],
            );
            let tx_buffer = udp::PacketBuffer::new(
                vec![udp::PacketMetadata::EMPTY; 8],
                vec![0; 65535],
            );
            udp::Socket::new(rx_buffer, tx_buffer)
        };

        let mut server_sockets = SocketSet::new(vec![]);
        let server_handle = server_sockets.add(server_socket);

        loop {
            let timestamp = Instant::now();
            server_iface.poll(timestamp, &mut server_device, &mut server_sockets);

            let socket = server_sockets.get_mut::<udp::Socket>(server_handle);
            if !socket.is_open() {
                socket.bind(6969).unwrap();
                println!("\x1b[31mReceiver socket bound to port 6969\x1b[0m");
            }

            let client = match socket.recv() {
                Ok((data, endpoint)) => {
                    debug!("udp:6969 recv data: {:?} from {}", data, endpoint);
                    let mut data = data.to_vec();
                    data.reverse();
                    Some((endpoint, data))
                }
                Err(_) => None,
            };

            if let Some((endpoint, data)) = client {
                debug!("udp:6969 send data: {:?} to {}", data, endpoint,);
                socket.send_slice(&data, endpoint).unwrap();
            }

            phy_wait(server_fd, server_iface.poll_delay(timestamp, &server_sockets))
                .expect("wait error");
        }
    });

    // 创建客户端线程
    let client_thread = thread::spawn(move || {
        println!("\x1b[33mClient thread started\x1b[0m");

        let server_endpoint = (IpAddress::v4(192, 168, 69, 1), 6969);
        let (mut client_iface, mut client_device) = create_network_stack(
            "tap0", 
            [192, 168, 69, 2], 
            EthernetAddress([0x02, 0x00, 0x00, 0x00, 0x00, 0x02]));
        let client_fd = client_device.as_raw_fd();

        // 创建客户端socket
        let client_socket = {
            let rx_buffer = udp::PacketBuffer::new(
                vec![udp::PacketMetadata::EMPTY; 8],
                vec![0; 65535],
            );
            let tx_buffer = udp::PacketBuffer::new(
                vec![udp::PacketMetadata::EMPTY; 8],
                vec![0; 65535],
            );
            udp::Socket::new(rx_buffer, tx_buffer)
        };

        let mut client_sockets = SocketSet::new(vec![]);
        let client_handle = client_sockets.add(client_socket);

        loop {
            let timestamp = Instant::now();
            client_iface.poll(timestamp, &mut client_device, &mut client_sockets);

            let socket = client_sockets.get_mut::<udp::Socket>(client_handle);
            if !socket.is_open() {
                socket.bind(7969).unwrap();
                println!("\x1b[33mSender socket bound to port 7969\x1b[0m");
            }

            let data = b"Hello from sender!";
            match socket.send_slice(data, server_endpoint) {
                Ok(_) => println!("Sent data {:?} to {:?}", data, server_endpoint),
                Err(e) => println!("Failed to send data: {}", e),
            }

            phy_wait(client_fd, client_iface.poll_delay(timestamp, &client_sockets))
                .expect("wait error");

            thread::sleep(Duration::from_secs(1));         
        }
    });

    server_thread.join().unwrap();
    client_thread.join().unwrap();
}