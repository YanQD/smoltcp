mod utils;

use log::info;
use std::collections::HashMap;
use std::os::unix::io::AsRawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use spin::Mutex;
use std::thread;
use smoltcp::time::Instant;

use smoltcp::iface::{Config, Interface, SocketSet};
use smoltcp::phy::{wait as phy_wait, Device, DeviceCapabilities, Medium, RxToken, TunTapInterface, TxToken};
use smoltcp::socket::udp;
use smoltcp::time::Instant as SmoltcpInstant;
use smoltcp::wire::{EthernetAddress, EthernetFrame, IpAddress, IpCidr, Ipv4Address, Ipv6Address};

// 交换机端口结构
struct SwitchPort {
    mac_addr: EthernetAddress,
    sender: Sender<Vec<u8>>,
}

// 交换机实现
struct Switch {
    mac_table: Arc<Mutex<HashMap<EthernetAddress, usize>>>,  // MAC地址表
    ports: Vec<SwitchPort>,                                  // 交换机端口
}

impl Switch {
    fn new() -> Self {
        Switch {
            mac_table: Arc::new(Mutex::new(HashMap::new())),
            ports: Vec::new(),
        }
    }

    // 添加端口
    fn add_port(&mut self, mac_addr: EthernetAddress, sender: Sender<Vec<u8>>) -> usize {
        let port_no = self.ports.len();
        self.ports.push(SwitchPort { mac_addr, sender });
        port_no
    }

    // 处理收到的帧
    fn process_frame(&mut self, frame: Vec<u8>, in_port: usize) {
        // 解析以太网帧
        if let Ok(frame) = EthernetFrame::new_checked(&frame) {
            let src_mac = frame.src_addr();
            let dst_mac = frame.dst_addr();

            println!("\nSwitch received frame on port {}:", in_port);
            println!("Src MAC: {}", src_mac);
            println!("Dst MAC: {}", dst_mac);

            // 学习源MAC地址
            self.mac_table.lock().insert(src_mac, in_port);

            // 查找目标端口并转发
            if dst_mac == EthernetAddress::BROADCAST {
                // 广播
                for (port_no, port) in self.ports.iter().enumerate() {
                    if port_no != in_port {
                        let _ = port.sender.send(frame.clone().into_inner().to_vec());
                    }
                }
            } else {
                // 单播
                if let Some(&out_port) = self.mac_table.lock().get(&dst_mac) {
                    if out_port != in_port {
                        let _ = self.ports[out_port].sender.send(frame.into_inner().to_vec());
                    }
                } else {
                    // 未知目标MAC，泛洪
                    for (port_no, port) in self.ports.iter().enumerate() {
                        if port_no != in_port {
                            let _ = port.sender.send(frame.clone().into_inner().to_vec());
                        }
                    }
                }
            }
        }
    }

    // 打印MAC地址表
    fn print_mac_table(&self) {
        let table = self.mac_table.lock();
        println!("\n=== MAC Address Table ===");
        for (mac, port) in table.iter() {
            println!("MAC: {}, Port: {}", mac, port);
        }
        println!("========================\n");
    }
}

// 帧捕获设备
struct FrameCapture {
    inner: Arc<Mutex<TunTapInterface>>,
    name: String,
    port_no: usize,
    frame_sender: Sender<(Vec<u8>, usize)>,  // 发送到交换机
    frame_receiver: Receiver<Vec<u8>>,       // 从交换机接收
}

impl FrameCapture {
    fn new(
        name: &str, 
        medium: Medium,
        frame_sender: Sender<(Vec<u8>, usize)>,
        frame_receiver: Receiver<Vec<u8>>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let inner = Arc::new(Mutex::new(TunTapInterface::new(name, medium)?));
        Ok(FrameCapture {
            inner,
            name: name.to_string(),
            port_no: 0,
            frame_sender,
            frame_receiver,
        })
    }

    fn set_port_no(&mut self, port_no: usize) {
        self.port_no = port_no;
    }

    fn as_raw_fd(&self) -> i32 {
        self.inner.lock().as_raw_fd()
    }
}

// 实现 Device trait
impl Device for FrameCapture {
    type RxToken<'a> = FrameCaptureRxToken;
    type TxToken<'a> = FrameCaptureTxToken<'a>;

    fn capabilities(&self) -> DeviceCapabilities {
        self.inner.lock().capabilities()
    }

    fn receive(&mut self, timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        // 检查是否有来自交换机的数据
        if let Ok(frame) = self.frame_receiver.try_recv() {
            println!("\n[{}] Received frame from switch", self.name);
            return Some((
                FrameCaptureRxToken {
                    buffer: frame,
                    name: self.name.clone(),
                },
                FrameCaptureTxToken {
                    inner: self.inner.lock().transmit(timestamp)?,
                    sender: self.frame_sender.clone(),
                    port_no: self.port_no,
                    name: self.name.clone(),
                },
            ));
        }

        // 检查设备是否有数据
        self.inner.lock().receive(timestamp).map(|(rx, tx)| {
            (
                FrameCaptureRxToken {
                    buffer: rx.consume(|buffer| buffer.to_vec()),
                    name: self.name.clone(),
                },
                FrameCaptureTxToken {
                    inner: tx,
                    sender: self.frame_sender.clone(),
                    port_no: self.port_no,
                    name: self.name.clone(),
                },
            )
        })
    }

    fn transmit(&mut self, timestamp: Instant) -> Option<Self::TxToken<'_>> {
        self.inner.lock().transmit(timestamp).map(|tx| {
            FrameCaptureTxToken {
                inner: tx,
                sender: self.frame_sender.clone(),
                port_no: self.port_no,
                name: self.name.clone(),
            }
        })
    }
}

// 接收令牌
struct FrameCaptureRxToken {
    buffer: Vec<u8>,
    name: String,
}

impl RxToken for FrameCaptureRxToken {
    fn consume<R, F>(mut self, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        println!("\n[{}] Processing received frame:", self.name);
        print_frame(&self.buffer);
        f(&mut self.buffer)
    }
}

// 发送令牌
struct FrameCaptureTxToken<'a> {
    inner: <TunTapInterface as Device>::TxToken<'a>,
    sender: Sender<(Vec<u8>, usize)>,
    port_no: usize,
    name: String,
}

impl<'a> TxToken for FrameCaptureTxToken<'a> {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        self.inner.consume(len, |buffer| {
            let result = f(buffer);
            println!("\n[{}] Transmitting frame:", self.name);
            print_frame(buffer);
            // 发送到交换机
            let _ = self.sender.send((buffer.to_vec(), self.port_no));
            result
        })
    }
}

// 帮助函数：打印帧内容
fn print_frame(buffer: &[u8]) {
    println!("Frame length: {} bytes", buffer.len());
    println!("Raw data:");
    for (i, byte) in buffer.iter().enumerate() {
        print!("{:02x} ", byte);
        if (i + 1) % 16 == 0 {
            println!();
        }
    }
    println!("\n");

    if buffer.len() >= 14 {
        if let Ok(frame) = EthernetFrame::new_checked(buffer) {
            println!("Ethernet Header:");
            println!("  Dst MAC: {}", frame.dst_addr());
            println!("  Src MAC: {}", frame.src_addr());
            println!("  Type: {:?}\n", frame.ethertype());
        }
    }
}

fn run_server(
    mut device: FrameCapture,
    running: Arc<AtomicBool>,
) -> Result<(), Box<dyn std::error::Error>> {
    let fd = device.as_raw_fd();

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
        vec![udp::PacketMetadata::EMPTY, udp::PacketMetadata::EMPTY],
        vec![0; 65535],
    );
    let udp_tx_buffer = udp::PacketBuffer::new(
        vec![udp::PacketMetadata::EMPTY, udp::PacketMetadata::EMPTY],
        vec![0; 65535],
    );
    let udp_socket = udp::Socket::new(udp_rx_buffer, udp_tx_buffer);

    let mut sockets = SocketSet::new(vec![]);
    let udp_handle = sockets.add(udp_socket);

    info!("Server started on 192.168.69.11:6969");

    while running.load(Ordering::SeqCst) {
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

        phy_wait(fd, iface.poll_delay(timestamp, &sockets))
            .expect("wait error");
    }

    Ok(())
}

fn run_client(
    mut device: FrameCapture,
    running: Arc<AtomicBool>,
) -> Result<(), Box<dyn std::error::Error>> {
    let fd = device.as_raw_fd();

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
        vec![udp::PacketMetadata::EMPTY, udp::PacketMetadata::EMPTY],
        vec![0; 65535],
    );
    let udp_tx_buffer = udp::PacketBuffer::new(
        vec![udp::PacketMetadata::EMPTY, udp::PacketMetadata::EMPTY],
        vec![0; 65535],
    );
    let udp_socket = udp::Socket::new(udp_rx_buffer, udp_tx_buffer);

    let mut sockets = SocketSet::new(vec![]);
    let udp_handle = sockets.add(udp_socket);

    info!("Client started on 192.168.69.22:7969");

    while running.load(Ordering::SeqCst) {
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
            Err(e) => println!("Failed to send data: {}", e),
        }

        if let Ok((data, endpoint)) = socket.recv() {
            info!("Client received: {:?} from {}", data, endpoint);
        }

        phy_wait(fd, iface.poll_delay(timestamp, &sockets))
            .expect("wait error");
    }

    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    utils::setup_logging("");

    // 创建一个运行标志
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();

    // 处理 Ctrl-C
    ctrlc::set_handler(move || {
        println!("Received Ctrl-C, shutting down...");
        r.store(false, Ordering::SeqCst);
    })?;

    // 创建交换机
    let mut switch = Switch::new();
    let (switch_sender, switch_receiver) = channel();

    // 为客户端创建通道
    let (client_sender, client_receiver) = channel();
    let mut client_capture = FrameCapture::new(
        "tap1",
        Medium::Ethernet,
        switch_sender.clone(),
        client_receiver,
    )?;

    // 为服务端创建通道
    let (server_sender, server_receiver) = channel();
    let mut server_capture = FrameCapture::new(
        "tap2",
        Medium::Ethernet,
        switch_sender.clone(),
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

    // 启动交换机线程
    let switch_running = running.clone();
    let switch_thread = thread::spawn(move || {
        while switch_running.load(Ordering::SeqCst) {
            if let Ok((frame, in_port)) = switch_receiver.recv() {
                switch.process_frame(frame, in_port);
                switch.print_mac_table();
            }
        }
    });

    // 启动服务端线程
    let server_running = running.clone();
    let server_thread = thread::spawn(move || {
        if let Err(e) = run_server(server_capture, server_running) {
            eprintln!("Server error: {}", e);
        }
    });

    // 启动客户端线程
    let client_running = running.clone();
    let client_thread = thread::spawn(move || {
        if let Err(e) = run_client(client_capture, client_running) {
            eprintln!("Client error: {}", e);
        }
    });

    // 等待线程完成
    server_thread.join().unwrap();
    client_thread.join().unwrap();
    switch_thread.join().unwrap();

    Ok(())
}