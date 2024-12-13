mod utils;

use log::debug;
use std::os::unix::io::AsRawFd;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use smoltcp::iface::{Config, Interface, SocketSet};
use smoltcp::phy::{wait as phy_wait, Device, DeviceCapabilities, Medium, RxToken, TunTapInterface, TxToken};
use smoltcp::socket::udp;
use smoltcp::time::Instant;
use smoltcp::wire::{EthernetAddress, IpAddress, IpCidr, Ipv4Address, Ipv6Address};

struct CapturedFrame {
    timestamp: Instant,
    direction: Direction,
    data: Vec<u8>,
}

#[derive(Debug)]
enum Direction {
    Tx,
    Rx,
}

struct FrameCapture {
    inner: TunTapInterface,
    frames: Arc<Mutex<Vec<CapturedFrame>>>,
}


impl FrameCapture {
    fn new(name: &str, medium: Medium) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(FrameCapture {
            frames: Arc::new(Mutex::new(Vec::new())),
            inner: TunTapInterface::new(name, medium)?,
        })
    }

    // 打印所有捕获的帧
    fn print_captured_frames(&self) {
        let frames = self.frames.lock().unwrap();
        println!("\n=== Captured Frames ===");
        println!("Total frames: {}", frames.len());
        
        for (i, frame) in frames.iter().enumerate() {
            println!("\nFrame #{}", i + 1);
            println!("Timestamp: {:?}", frame.timestamp);
            println!("Direction: {:?}", frame.direction);
            println!("Data length: {}", frame.data.len());
            println!("Raw data: {:?}", frame.data);
        }
        println!("=== End of Captured Frames ===\n");
    }
    
    fn as_raw_fd(&self) -> i32 {
        self.inner.as_raw_fd()
    }
}

// 实现 Device trait
impl Device for FrameCapture {
    type RxToken<'a> = FrameCaptureRxToken<'a> where Self: 'a; 
    type TxToken<'a> = FrameCaptureTxToken<'a> where Self: 'a;

    fn capabilities(&self) -> DeviceCapabilities {
        self.inner.capabilities()
    }

    fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        // 处理从设备接收的数据
        let frames = self.frames.clone();
        self.inner.receive(_timestamp).map(|(rx, tx)| {
            (
                FrameCaptureRxToken { 
                    inner: rx,
                    device_frames: frames.clone(),
                },
                FrameCaptureTxToken { 
                    inner: tx,
                    device_frames: frames,
                }
            )
        })
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        let frames = self.frames.clone();
        self.inner.transmit(_timestamp).map(|tx| {
            FrameCaptureTxToken { 
                inner: tx,
                device_frames: frames,
            }
        })
    }
}

// 包装接收令牌
struct FrameCaptureRxToken<'a> {
    inner: <TunTapInterface as Device>::RxToken<'a>,
    device_frames: Arc<Mutex<Vec<CapturedFrame>>>,
}

impl<'a> RxToken for FrameCaptureRxToken<'a> {
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        self.inner.consume(|buffer| {
            // 保存接收到的帧
            let frame = CapturedFrame {
                timestamp: Instant::now(),
                direction: Direction::Rx,
                data: buffer.to_vec(),
            };
            self.device_frames.lock().unwrap().push(frame);

            println!("Received frame:");
            print_frame(buffer);
            f(buffer)
        })
    }
}

// 包装发送令牌
struct FrameCaptureTxToken<'a> {
    inner: <TunTapInterface as Device>::TxToken<'a>,
    device_frames: Arc<Mutex<Vec<CapturedFrame>>>,
}

impl<'a> TxToken for FrameCaptureTxToken<'a> {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        self.inner.consume(len, |buffer| {
            let result = f(buffer);

            // 保存发送的帧
            let frame = CapturedFrame {
                timestamp: Instant::now(),
                direction: Direction::Tx,
                data: buffer.to_vec(),
            };
            self.device_frames.lock().unwrap().push(frame);

            println!("Transmitting frame:");
            print_frame(buffer);

            println!("Captured frames:");
            for frame in self.device_frames.lock().unwrap().iter() {
                println!("Timestamp: {:?}, Direction: {:?}, Data: {:?}", frame.timestamp, frame.direction, frame.data);
            }
            result
        })
    }
}

// 帮助函数：打印帧内容
fn print_frame(buffer: &[u8]) {
    println!("Frame length: {} bytes", buffer.len());
    println!("\x1b[36mRaw data: {:?}\x1b[0m", buffer);
    if buffer.len() >= 14 {  // 以太网头部是14字节
        println!("Ethernet Header:");
        println!("Dest MAC: {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            buffer[0], buffer[1], buffer[2], buffer[3], buffer[4], buffer[5]);
        println!("Src MAC: {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            buffer[6], buffer[7], buffer[8], buffer[9], buffer[10], buffer[11]);
        println!("Type: {:02x}{:02x}",
            buffer[12], buffer[13]);
    }
}

fn main() {
    utils::setup_logging("");

    // 创建服务端线程
    let server_thread = thread::spawn(move || {
        let mut device1 = FrameCapture::new("tap1", Medium::Ethernet).unwrap();
        let fd1 = device1.as_raw_fd();

        // Create interface
        let mut config1 = Config::new(EthernetAddress([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]).into());

        config1.random_seed = rand::random();

        let mut iface1 = Interface::new(config1, &mut device1, Instant::now());
        iface1.update_ip_addrs(|ip_addrs| {
            ip_addrs
                .push(IpCidr::new(IpAddress::v4(192, 168, 69, 11), 24))
                .unwrap();
            ip_addrs
                .push(IpCidr::new(IpAddress::v6(0xfdaa, 0, 0, 0, 0, 0, 0, 0x11), 64))
                .unwrap();
        });
        iface1
            .routes_mut()
            .add_default_ipv4_route(Ipv4Address::new(192, 168, 69, 22))
            .unwrap();
        iface1
            .routes_mut()
            .add_default_ipv6_route(Ipv6Address::new(0xfe80, 0, 0, 0, 0, 0, 0, 0x22))
            .unwrap();

        // Create sockets
        let udp_rx_buffer1 = udp::PacketBuffer::new(
            vec![udp::PacketMetadata::EMPTY, udp::PacketMetadata::EMPTY],
            vec![0; 65535],
        );
        let udp_tx_buffer1 = udp::PacketBuffer::new(
            vec![udp::PacketMetadata::EMPTY, udp::PacketMetadata::EMPTY],
            vec![0; 65535],
        );
        let udp_socket1 = udp::Socket::new(udp_rx_buffer1, udp_tx_buffer1);

        let mut sockets1 = SocketSet::new(vec![]);
        let udp_handle1 = sockets1.add(udp_socket1);

        loop {
            println!("\x1b[35mServer loop\x1b[0m");
            {   
                let timestamp = Instant::now();
                iface1.poll(timestamp, &mut device1, &mut sockets1);

                // udp:6969: respond "hello"
                let socket = sockets1.get_mut::<udp::Socket>(udp_handle1);
                if !socket.is_open() {
                    socket.bind(6969).unwrap()
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

                phy_wait(fd1, iface1.poll_delay(timestamp, &sockets1))
                    .expect("wait error");        
            }
            thread::sleep(Duration::from_millis(10));
            device1.print_captured_frames();
        }
    });

    // 创建客户端线程
    let client_thread = thread::spawn(move || {
        let mut device2 = FrameCapture::new("tap2", Medium::Ethernet).unwrap();
        let fd2 = device2.as_raw_fd();

        // Create interface
        let mut config2 = Config::new(EthernetAddress([0x02, 0x00, 0x00, 0x00, 0x00, 0x02]).into());

        config2.random_seed = rand::random();

        let mut iface2 = Interface::new(config2, &mut device2, Instant::now());
        iface2.update_ip_addrs(|ip_addrs| {
            ip_addrs
                .push(IpCidr::new(IpAddress::v4(192, 168, 69, 22), 24))
                .unwrap();
            ip_addrs
                .push(IpCidr::new(IpAddress::v6(0xfdaa, 0, 0, 0, 0, 0, 0, 0x22), 64))
                .unwrap();
        });
        iface2
            .routes_mut()
            .add_default_ipv4_route(Ipv4Address::new(192, 168, 69, 11))
            .unwrap();
        iface2
            .routes_mut()
            .add_default_ipv6_route(Ipv6Address::new(0xfe80, 0, 0, 0, 0, 0, 0, 0x11))
            .unwrap();

        // Create sockets
        let udp_rx_buffer2 = udp::PacketBuffer::new(
            vec![udp::PacketMetadata::EMPTY, udp::PacketMetadata::EMPTY],
            vec![0; 65535],
        );
        let udp_tx_buffer2 = udp::PacketBuffer::new(
            vec![udp::PacketMetadata::EMPTY, udp::PacketMetadata::EMPTY],
            vec![0; 65535],
        );
        let udp_socket2 = udp::Socket::new(udp_rx_buffer2, udp_tx_buffer2);

        let mut sockets2 = SocketSet::new(vec![]);
        let udp_handle2 = sockets2.add(udp_socket2);

        loop {
            println!("\x1b[35mClient loop\x1b[0m");
            {
                let timestamp = Instant::now();
                iface2.poll(timestamp, &mut device2, &mut sockets2);

                let socket = sockets2.get_mut::<udp::Socket>(udp_handle2);
                if !socket.is_open() {
                    socket.bind(7969).unwrap();
                    println!("\x1b[33mSender socket bound to port 7969\x1b[0m");
                }

                let server_endpoint = (IpAddress::v4(192, 168, 69, 11), 6969);
                let data = b"Hello from sender!";
                match socket.send_slice(data, server_endpoint) {
                    Ok(_) => println!("Client: Sent data {:?} to {:?}", data, server_endpoint),
                    Err(e) => println!("Failed to send data: {}", e),
                }

                if let Ok((data, endpoint)) = socket.recv() {
                    println!("\x1b[34mClient received response: {:?} from {}\x1b[0m", data, endpoint);
                }

                phy_wait(fd2, iface2.poll_delay(timestamp, &sockets2))
                    .expect("wait error");
            }
            thread::sleep(Duration::from_millis(10));
            device2.print_captured_frames();
        }
    });
    
    server_thread.join().unwrap();
    client_thread.join().unwrap();
}