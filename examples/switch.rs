mod utils;

use log::{debug, info};
use std::collections::HashMap;
use std::os::unix::io::AsRawFd;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::cell::UnsafeCell;
use std::ptr;
use spin::Mutex;
use std::thread;
use smoltcp::time::Instant;

use smoltcp::iface::{Config, Interface, SocketSet};
use smoltcp::phy::{wait as phy_wait, Device, DeviceCapabilities, Medium, RxToken, TunTapInterface, TxToken};
use smoltcp::socket::udp;
use smoltcp::time::Instant as SmoltcpInstant;
use smoltcp::wire::{EthernetAddress, EthernetFrame, IpAddress, IpCidr, Ipv4Address, Ipv6Address};

// 无锁环形缓冲区实现
#[derive(Debug)]
pub struct RingBuffer<T> {
    buffer: Box<[UnsafeCell<T>]>,
    head: AtomicUsize,
    tail: AtomicUsize,
    capacity: usize,
}

unsafe impl<T: Send> Send for RingBuffer<T> {}
unsafe impl<T: Send> Sync for RingBuffer<T> {}

#[derive(Clone, Debug)]
pub struct Producer<T> {
    buffer: Arc<RingBuffer<T>>,
}

#[derive(Clone, Debug)]
pub struct Consumer<T> {
    buffer: Arc<RingBuffer<T>>,
}

//
impl<T> RingBuffer<T> {
    pub fn new(capacity: usize) -> Self {
        let mut buffer = Vec::with_capacity(capacity);
        unsafe {
            buffer.set_len(capacity);
        }
        
        RingBuffer {
            buffer: buffer.into_boxed_slice(),
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
            capacity,
        }
    }

    pub fn split(self) -> (Producer<T>, Consumer<T>) {
        let buffer = Arc::new(self);
        (
            Producer {
                buffer: buffer.clone(),
            },
            Consumer {
                buffer,
            }
        )
    }
}

impl<T> Producer<T> {
    pub fn try_push(&self, value: T) -> Result<(), T> {
        let buffer = &self.buffer;
        let tail = buffer.tail.load(Ordering::Relaxed);
        let next_tail = (tail + 1) % buffer.capacity;
        let head = buffer.head.load(Ordering::Acquire);
        
        // println!("RingBuffer try_push: head={}, tail={}, next_tail={}, capacity={}", 
        //     head, tail, next_tail, buffer.capacity);
        
        // 如果是Vec<u8>类型,打印帧内容
        if let Some(frame_data) = unsafe { (&value as *const T).cast::<Vec<u8>>().as_ref() } {
            println!("\nPushing frame data:");
            print_ethernet_frame(frame_data);
        }
        
        if next_tail == head {
            println!("RingBuffer is full!");
            return Err(value);
        }

        unsafe {
            (*buffer.buffer[tail].get()) = value;
        }
        
        buffer.tail.store(next_tail, Ordering::Release);
        println!("RingBuffer push success: new tail={}", next_tail);

        // 如果是 Vec<u8> 类型,打印帧内容
        for i in 0..buffer.tail.load(Ordering::Relaxed) {
            if let Some(frame_data) = unsafe { buffer.buffer[i].get().cast::<Vec<u8>>().as_ref() } {
                debug!("Pushed frame data:");
                debug!("frame_data: {:?}", frame_data);
                // print_ethernet_frame(frame_data);
            }
        }

        Ok(())
    }
}

impl<T> Consumer<T> {
    pub fn try_pop(&self) -> Option<T> {
        let buffer = &self.buffer;
        let head = buffer.head.load(Ordering::Relaxed);
        let tail = buffer.tail.load(Ordering::Acquire);
        
        // debug!("\x1b[35mtest test test\x1b[0m");
        // println!("RingBuffer try_pop: head={}, tail={}, capacity={}", 
        //     head, tail, buffer.capacity);

        if head == tail {
            // println!("RingBuffer is empty!");
            return None;
        }

        let value = unsafe {
            ptr::read(buffer.buffer[head].get())
        };
        
        // 如果是Vec<u8>类型，打印帧内容
        if let Some(frame_data) = unsafe { (&value as *const T).cast::<Vec<u8>>().as_ref() } {
            println!("\nPopped frame data:");
            print_ethernet_frame(frame_data);
        }

        let next_head = (head + 1) % buffer.capacity;
        buffer.head.store(next_head, Ordering::Release);
        println!("RingBuffer pop success: new head={}", next_head);
        
        Some(value)
    }
}

// 端口发送者和接收者
pub struct PortSender {
    producer: Producer<Vec<u8>>,
}

impl PortSender {
    pub fn send(&self, data: Vec<u8>) -> Result<(), Vec<u8>> {
        let result = self.producer.try_push(data);
        result
    }
}

pub struct PortReceiver {
    consumer: Consumer<Vec<u8>>,
}

impl PortReceiver {
    pub fn try_recv(&self) -> Option<Vec<u8>> {
        let result = self.consumer.try_pop();
        println!("PortReceiver try_recv: {:?}", result);
        result
    }
}

// 创建端口通道
pub fn create_port_channel(capacity: usize) -> (PortSender, PortReceiver) {
    let ring_buffer = RingBuffer::new(capacity);
    let (producer, consumer) = ring_buffer.split();
    (
        PortSender { producer },
        PortReceiver { consumer }
    )
}

// 交换机相关结构
struct SwitchPort {
    #[allow(dead_code)]
    mac_addr: EthernetAddress,
    sender: PortSender,
}

struct Switch {
    mac_table: Arc<Mutex<HashMap<EthernetAddress, usize>>>,
    ports: Vec<SwitchPort>,
}

impl Switch {
    fn new() -> Self {
        Switch {
            mac_table: Arc::new(Mutex::new(HashMap::new())),
            ports: Vec::new(),
        }
    }

    fn add_mac_list(&mut self, mac_addr: EthernetAddress, port_no: usize) {
        self.mac_table.lock().insert(mac_addr, port_no);
    }

    fn add_port(&mut self, mac_addr: EthernetAddress, sender: PortSender) -> usize {
        let port_no = self.ports.len();
        self.ports.push(SwitchPort { mac_addr, sender });
        port_no
    }

    fn process_frame(&mut self, frame: Vec<u8>, in_port: usize) {
        println!("\nSwitch processing frame of size {} from port {}", frame.len(), in_port);
        
        if let Ok(frame) = EthernetFrame::new_checked(&frame) {
            let src_mac = frame.src_addr();
            let dst_mac = frame.dst_addr();
            
            println!("\nSwitch: received frame on port {}", in_port);
            println!("Source MAC: {}", src_mac);
            println!("Destination MAC: {}", dst_mac);
            
            // 更新MAC地址表
            self.mac_table.lock().insert(src_mac, in_port);
            
            let frame_data = frame.into_inner().to_vec();
            
            // 转发决策
            if dst_mac == EthernetAddress::BROADCAST {
                println!("Switch: broadcasting frame");
                for (port_no, port) in self.ports.iter().enumerate() {
                    if port_no != in_port {
                        println!("Switch: sending to port {}", port_no);
                        if let Err(e) = port.sender.send(frame_data.clone()) {
                            println!("Switch: failed to send to port {}: {:?}", port_no, e);
                        }
                    }
                }
            } else {
                // 查找目标端口
                if let Some(&out_port) = self.mac_table.lock().get(&dst_mac) {
                    if out_port != in_port {
                        println!("Switch: forwarding to port {}", out_port);
                        if let Err(e) = self.ports[out_port].sender.send(frame_data) {
                            println!("Switch: failed to forward to port {}: {:?}", out_port, e);
                        }
                    }
                } else {
                    println!("Switch: flooding frame (unknown destination)");
                    for (port_no, port) in self.ports.iter().enumerate() {
                        if port_no != in_port {
                            if let Err(e) = port.sender.send(frame_data.clone()) {
                                println!("Switch: failed to flood to port {}: {:?}", port_no, e);
                            }
                        }
                    }
                }
            }
        } else {
            println!("Invalid ethernet frame received!");
        }
    }

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
    // 发送通道：发送 (数据包, 源端口号) 到交换机
    frame_sender: Producer<(Vec<u8>, usize)>,
    // 接收通道：从交换机接收数据包
    frame_receiver: PortReceiver,
}

impl FrameCapture {
    fn new(
        name: &str,
        medium: Medium,
        frame_sender: Producer<(Vec<u8>, usize)>,
        frame_receiver: PortReceiver,
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

impl Device for FrameCapture {
    type RxToken<'a> = FrameCaptureRxToken;
    type TxToken<'a> = FrameCaptureTxToken<'a>;

    fn capabilities(&self) -> DeviceCapabilities {
        self.inner.lock().capabilities()
    }

    fn receive(&mut self, timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        println!("\n[{}] Checking for frames from switch", self.name);
        // 优先检查是否有来自交换机的数据

        if let Some(frame) = self.frame_receiver.try_recv() {
            println!("frame {:?}!", frame);
            println!("\n[{}] Received frame from switch, writing to tap", self.name);
            // 直接写入到tap设备
            return Some((
                FrameCaptureRxToken {
                    buffer: frame,
                    name: self.name.clone(),
                },
                FrameCaptureTxToken {
                    inner: self.inner.lock().transmit(timestamp).unwrap(),
                    sender: &self.frame_sender,
                    port_no: self.port_no,
                    name: self.name.clone(),
                }
            ));
        } else {
            println!("[{}] No frames from switch", self.name);
        }
    
        // 然后检查tap设备是否有新数据
        let result = self.inner.lock().receive(timestamp).map(|(rx, tx)| {
            (
                FrameCaptureRxToken {
                    buffer: rx.consume(|buffer| buffer.to_vec()),
                    name: self.name.clone(),
                },
                FrameCaptureTxToken {
                    inner: tx,
                    sender: &self.frame_sender,
                    port_no: self.port_no,
                    name: self.name.clone(),
                },
            )
        });

        if result.is_none() {
            println!("[{}] No frames from tap device", self.name);
        }

        result
    }

    fn transmit(&mut self, timestamp: Instant) -> Option<Self::TxToken<'_>> {
        self.inner.lock().transmit(timestamp).map(|tx| {
            FrameCaptureTxToken {
                inner: tx,
                sender: &self.frame_sender,
                port_no: self.port_no,
                name: self.name.clone(),
            }
        })
    }
}

// 接收
struct FrameCaptureRxToken {
    buffer: Vec<u8>,
    name: String,
}

impl RxToken for FrameCaptureRxToken {
    fn consume<R, F>(mut self, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        println!("\n[{}] Processing received frame of size: {}", self.name, self.buffer.len());
        // print_frame(&self.buffer);
        f(&mut self.buffer)
    }
}

// 发送
struct FrameCaptureTxToken<'a> {
    inner: <TunTapInterface as Device>::TxToken<'a>,
    sender: &'a Producer<(Vec<u8>, usize)>,
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
            println!("FrameCaptureTxToken consume: {:?}", buffer);
            println!("\n[{}] Sending frame to switch from port {}", self.name, self.port_no);
            let _ = self.sender.try_push((buffer.to_vec(), self.port_no));
            result
        })
    }
}

// 帮助函数：打印以太网帧内容
fn print_ethernet_frame(buffer: &[u8]) {
    println!("\nFrame length: {} bytes", buffer.len());
    println!("Raw data:");
    for (i, byte) in buffer.iter().enumerate() {
        print!("{:02x} ", byte);
        if (i + 1) % 16 == 0 {
            println!();
        }
    }
    println!();

    if buffer.len() >= 14 {
        // 尝试解析以太网帧头
        if let Ok(frame) = EthernetFrame::new_checked(buffer) {
            println!("Ethernet Header:");
            println!("  Dst MAC: {}", frame.dst_addr());
            println!("  Src MAC: {}", frame.src_addr());
            println!("  Type: {:?}", frame.ethertype());
            
            // 打印负载长度
            println!("  Payload length: {} bytes", frame.payload().len());
            
            // 简单打印前32字节的负载(如果有)
            let payload = frame.payload();
            if !payload.is_empty() {
                println!("  Payload preview (first 32 bytes):");
                for (i, byte) in payload.iter().take(32).enumerate() {
                    print!("{:02x} ", byte);
                    if (i + 1) % 16 == 0 {
                        println!();
                    }
                }
                println!();
            }
        } else {
            println!("Failed to parse ethernet frame!");
        }
    } else {
        println!("Buffer too short for ethernet frame!");
    }
    println!("----------------------------------------");
}

fn run_server(
    mut device: FrameCapture,
) -> Result<(), Box<dyn std::error::Error>> {
    // let fd = device.as_raw_fd();

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

        // phy_wait(fd, iface.poll_delay(timestamp, &sockets))
        //     .expect("wait error");
        thread::sleep(std::time::Duration::from_micros(1000));
    };
}

fn run_client(
    mut device: FrameCapture,
) -> Result<(), Box<dyn std::error::Error>> {
    // let fd = device.as_raw_fd();

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

        // phy_wait(fd, iface.poll_delay(timestamp, &sockets))
        //     .expect("wait error");
        thread::sleep(std::time::Duration::from_micros(1000));
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    utils::setup_logging("");

    // 创建交换机
    let mut switch = Switch::new();
    
    // 创建用于向交换机发送数据的环形缓冲区
    let switch_buffer = RingBuffer::new(16);
    let (switch_producer, switch_consumer) = switch_buffer.split();

    // 为客户端创建通道
    let (client_sender, client_receiver) = create_port_channel(16);
    let mut client_capture = FrameCapture::new(
        "tap2",
        Medium::Ethernet,
        switch_producer.clone(),
        client_receiver,
    )?;

    // 为服务端创建通道
    let (server_sender, server_receiver) = create_port_channel(16);
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
            while let Some((frame, in_port)) = switch_consumer.try_pop() {
                println!("Switch processing frame from port {}", in_port);
                switch.process_frame(frame, in_port);
                switch.print_mac_table();
            }
            // 减少睡眠时间以提高响应性
            thread::sleep(std::time::Duration::from_micros(1000));
        }
    });

    // 等待线程完成
    server_thread.join().unwrap();
    client_thread.join().unwrap();
    switch_thread.join().unwrap();

    Ok(())
}