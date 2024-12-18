mod utils;
mod switch;
mod frame;
mod bridge_device;

use frame::FrameCapture;
use log::info;
use switch::{create_port_channel, RingBuffer, Switch};

use std::thread;

use smoltcp::iface::{Config, Interface, SocketSet};
use smoltcp::phy::Medium;
use smoltcp::socket::udp;
use smoltcp::time::Instant as SmoltcpInstant;
use smoltcp::wire::{EthernetAddress, IpAddress, IpCidr, Ipv4Address, Ipv6Address};

fn run_server(
    mut device: FrameCapture,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut config = Config::new(EthernetAddress([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]).into());
    config.random_seed = rand::random();

    let mut iface = Interface::new(config, &mut device, SmoltcpInstant::now());
    iface.update_ip_addrs(|ip_addrs| {
        ip_addrs
            .push(IpCidr::new(IpAddress::v4(192, 168, 69, 11), 24))
            .unwrap();
        ip_addrs
            .push(IpCidr::new(IpAddress::v6(0xfdaa, 0, 0, 0, 0, 0, 0, 0x11), 64))
            .unwrap();
    });

    iface.routes_mut()
        .add_default_ipv4_route(Ipv4Address::new(192, 168, 69, 22))
        .unwrap();
    iface.routes_mut()
        .add_default_ipv6_route(Ipv6Address::new(0xfe80, 0, 0, 0, 0, 0, 0, 0x22))
        .unwrap();

    let udp_rx_buffer = udp::PacketBuffer::new(
        vec![udp::PacketMetadata::EMPTY; 16],
        vec![0; 65535],
    );
    let udp_tx_buffer = udp::PacketBuffer::new(
        vec![udp::PacketMetadata::EMPTY; 16],
        vec![0; 65535],
    );
    let udp_socket = udp::Socket::new(udp_rx_buffer, udp_tx_buffer);

    let mut sockets = SocketSet::new(vec![]);
    let udp_handle = sockets.add(udp_socket);

    info!("Server started on 192.168.69.11:6969");

    loop {
        println!("\x1b[34m------------------Server loop------------------\x1b[0m");
        let timestamp = SmoltcpInstant::now();

        iface.poll(timestamp, &mut device, &mut sockets);

        let socket = sockets.get_mut::<udp::Socket>(udp_handle);
        if !socket.is_open() {
            socket.bind(6969).unwrap();
            info!("Server socket bound to port 6969");
        }

        if let Ok((data, endpoint)) = socket.recv() {
            info!("Server received: {:?} from {}", data, endpoint);
            let mut response = data.to_vec();
            response.reverse();
            socket.send_slice(&response, endpoint).unwrap();
            info!("Server sent response: {:?} to {}", response, endpoint);
        }

        thread::sleep(std::time::Duration::from_millis(100));
    }
}

fn run_client(
    mut device: FrameCapture,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut config = Config::new(EthernetAddress([0x02, 0x00, 0x00, 0x00, 0x00, 0x02]).into());
    config.random_seed = rand::random();

    let mut iface = Interface::new(config, &mut device, SmoltcpInstant::now());
    iface.update_ip_addrs(|ip_addrs| {
        ip_addrs
            .push(IpCidr::new(IpAddress::v4(192, 168, 69, 22), 24))
            .unwrap();
        ip_addrs
            .push(IpCidr::new(IpAddress::v6(0xfdaa, 0, 0, 0, 0, 0, 0, 0x22), 64))
            .unwrap();
    });

    iface.routes_mut()
        .add_default_ipv4_route(Ipv4Address::new(192, 168, 69, 11))
        .unwrap();
    iface.routes_mut()
        .add_default_ipv6_route(Ipv6Address::new(0xfe80, 0, 0, 0, 0, 0, 0, 0x11))
        .unwrap();

    let udp_rx_buffer = udp::PacketBuffer::new(
        vec![udp::PacketMetadata::EMPTY; 16],
        vec![0; 65535],
    );
    let udp_tx_buffer = udp::PacketBuffer::new(
        vec![udp::PacketMetadata::EMPTY; 16],
        vec![0; 65535],
    );
    let udp_socket = udp::Socket::new(udp_rx_buffer, udp_tx_buffer);

    let mut sockets = SocketSet::new(vec![]);
    let udp_handle = sockets.add(udp_socket);

    info!("Client started on 192.168.69.22:7969");

    loop {
        println!("\x1b[34m------------------Client loop------------------\x1b[0m");
        let timestamp = SmoltcpInstant::now();

        iface.poll(timestamp, &mut device, &mut sockets);

        let socket = sockets.get_mut::<udp::Socket>(udp_handle);
        if !socket.is_open() {
            socket.bind(7969).unwrap();
            info!("Client socket bound to port 7969");
        }

        let server_endpoint = (IpAddress::v4(192, 168, 69, 11), 6969);
        let data = b"Hello from sender!";
        match socket.send_slice(data, server_endpoint) {
            Ok(_) => info!("Client sent: {:?} to {:?}", data, server_endpoint),
            Err(e) => println!("Failed to send datas: {}", e),
        }

        if let Ok((data, endpoint)) = socket.recv() {
            info!("Client received: {:?} from {}", data, endpoint);
        }

        thread::sleep(std::time::Duration::from_millis(100));
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    utils::setup_logging("");

    // 创建交换机
    let mut switch = Switch::new();
    
    // 创建用于向交换机发送数据的环形缓冲区
    let switch_buffer = RingBuffer::new();
    let (switch_producer, switch_consumer) = switch_buffer.split();

    // 为客户端创建通道
    let (client_sender, client_receiver) = create_port_channel();
    let mut client_capture = FrameCapture::new(
        "tap2",
        Medium::Ethernet,
        switch_producer.clone(),
        client_receiver,
    )?;

    // 为服务端创建通道
    let (server_sender, server_receiver) = create_port_channel();
    let mut server_capture = FrameCapture::new(
        "tap1",
        Medium::Ethernet,
        switch_producer.clone(),
        server_receiver,
    )?;

    // 添加端口到交换机
    let client_port = switch.add_port(
        EthernetAddress([0x02, 0x00, 0x00, 0x00, 0x00, 0x02]),
        client_sender,
    );
    let server_port = switch.add_port(
        EthernetAddress([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]),
        server_sender,
    );

    // 设置端口号
    client_capture.set_port_no(client_port);
    server_capture.set_port_no(server_port);

    // 预先添加MAC地址表项
    switch.add_mac_list(EthernetAddress([0x02, 0x00, 0x00, 0x00, 0x00, 0x02]), client_port);
    switch.add_mac_list(EthernetAddress([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]), server_port);

    // 服务端线程
    let server_thread = thread::spawn(move || {
        if let Err(e) = run_server(server_capture) {
            eprintln!("Server error: {}", e);
        }
    });

    // 客户端线程
    let client_thread = thread::spawn(move || {
        if let Err(e) = run_client(client_capture) {
            eprintln!("Client error: {}", e);
        }
    });

    // 交换机线程
    let switch_thread = thread::spawn(move || {
        loop {
            if let Some((frame, in_port)) = switch_consumer.try_pop_switch_frame() {
                println!("Switch processing frame from port {}", in_port);
                switch.process_frame(frame, in_port);
                switch.print_mac_table();
            }
            thread::sleep(std::time::Duration::from_micros(1000));
        }
    });

    // 等待线程完成
    server_thread.join().unwrap();
    client_thread.join().unwrap();
    switch_thread.join().unwrap();

    Ok(())
}