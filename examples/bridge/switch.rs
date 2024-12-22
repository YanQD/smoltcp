use std::{collections::HashMap, sync::{Arc, Mutex}};
use smoltcp::wire::{EthernetAddress, EthernetFrame};

// 使用枚举区分两种数据类型
#[derive(Debug)]
enum BufferData {
    Frame(Vec<u8>),
    SwitchFrame((Vec<u8>, usize)),
}

#[derive(Debug)]
pub struct RingBuffer {
    buffer: Arc<Mutex<Option<BufferData>>>,
}

#[derive(Clone, Debug)]
pub struct Producer {
    buffer: Arc<Mutex<Option<BufferData>>>,
}

#[derive(Clone, Debug)]
pub struct Consumer {
    buffer: Arc<Mutex<Option<BufferData>>>,
}

impl RingBuffer {
    pub fn new() -> Self {
        RingBuffer {
            buffer: Arc::new(Mutex::new(None)),
        }
    }

    pub fn split(self) -> (Producer, Consumer) {
        (
            Producer {
                buffer: self.buffer.clone(),
            },
            Consumer {
                buffer: self.buffer,
            }
        )
    }
}

impl Producer {
    // 发送普通数据包
    pub fn try_push_frame(&self, value: Vec<u8>) -> Result<(), Vec<u8>> {
        let mut buffer = self.buffer.lock().unwrap();
        if buffer.is_some() {
            println!("Buffer is full!");
            return Err(value);
        }
        
        *buffer = Some(BufferData::Frame(value));
        println!("Push frame success!");
        Ok(())
    }

    // 发送带端口号的数据包
    pub fn try_push_switch_frame(&self, value: (Vec<u8>, usize)) -> Result<(), (Vec<u8>, usize)> {
        let mut buffer = self.buffer.lock().unwrap();
        if buffer.is_some() {
            println!("Buffer is full!");
            return Err(value);
        }
        
        *buffer = Some(BufferData::SwitchFrame(value));
        println!("Push switch frame success!");
        Ok(())
    }
}

impl Consumer {
    // 获取普通数据包
    pub fn try_pop_frame(&self) -> Option<Vec<u8>> {
        let mut buffer = self.buffer.lock().unwrap();
        if let Some(BufferData::Frame(data)) = buffer.take() {
            println!("\nPopped frame data:");
            print_ethernet_frame(&data);
            println!("Pop frame success!");
            Some(data)
        } else {
            None
        }
    }

    // 获取带端口号的数据包
    pub fn try_pop_switch_frame(&self) -> Option<(Vec<u8>, usize)> {
        let mut buffer = self.buffer.lock().unwrap();
        if let Some(BufferData::SwitchFrame(data)) = buffer.take() {
            println!("\nPopped switch frame data:");
            print_ethernet_frame(&data.0);
            println!("From port: {}", data.1);
            println!("Pop switch frame success!");
            Some(data)
        } else {
            None
        }
    }
}

// 端口发送者和接收者
pub struct PortSender {
    producer: Producer,
}

impl PortSender {
    pub fn send(&self, data: Vec<u8>) -> Result<(), Vec<u8>> {
        self.producer.try_push_frame(data)
    }
}

pub struct PortReceiver {
    consumer: Consumer,
}

impl PortReceiver {
    pub fn try_recv(&self) -> Option<Vec<u8>> {
        self.consumer.try_pop_frame()
    }
}

// 创建端口通道
pub fn create_port_channel() -> (PortSender, PortReceiver) {
    let ring_buffer = RingBuffer::new();
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

pub struct Switch {
    mac_table: Arc<spin::Mutex<HashMap<EthernetAddress, usize>>>,
    ports: Vec<SwitchPort>,
}

impl Switch {
    pub fn new() -> Self {
        Switch {
            mac_table: Arc::new(spin::Mutex::new(HashMap::new())),
            ports: Vec::new(),
        }
    }

    pub fn add_mac_list(&mut self, mac_addr: EthernetAddress, port_no: usize) {
        self.mac_table.lock().insert(mac_addr, port_no);
    }

    pub fn add_port(&mut self, mac_addr: EthernetAddress, sender: PortSender) -> usize {
        let port_no = self.ports.len();
        self.ports.push(SwitchPort { mac_addr, sender });
        port_no
    }

    pub fn process_frame(&mut self, frame: Vec<u8>, in_port: usize) {
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

    pub fn print_mac_table(&self) {
        let table = self.mac_table.lock();
        println!("\n=== MAC Address Table ===");
        for (mac, port) in table.iter() {
            println!("MAC: {}, Port: {}", mac, port);
        }
        println!("========================\n");
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
        if let Ok(frame) = EthernetFrame::new_checked(buffer) {
            println!("Ethernet Header:");
            println!("  Dst MAC: {}", frame.dst_addr());
            println!("  Src MAC: {}", frame.src_addr());
            println!("  Type: {:?}", frame.ethertype());
            
            println!("  Payload length: {} bytes", frame.payload().len());
            
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