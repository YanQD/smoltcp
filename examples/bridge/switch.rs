use std::{cell::UnsafeCell, collections::HashMap, ptr, sync::{atomic::{AtomicUsize, Ordering}, Arc}};

use log::debug;
use smoltcp::wire::{EthernetAddress, EthernetFrame};
use spin::Mutex;

use crate::frame::print_ethernet_frame;

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
        
        println!("RingBuffer try_push: head={}, tail={}, next_tail={}, capacity={}", 
            head, tail, next_tail, buffer.capacity);
        
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
            } else {
                debug!("Pushed data: {:?}", unsafe { buffer.buffer[i].get() });
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
        
        debug!("\x1b[35mtest test test\x1b[0m");
        println!("RingBuffer try_pop: head={}, tail={}, capacity={}", 
            head, tail, buffer.capacity);

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

pub struct Switch {
    mac_table: Arc<Mutex<HashMap<EthernetAddress, usize>>>,
    ports: Vec<SwitchPort>,
}

impl Switch {
    pub fn new() -> Self {
        Switch {
            mac_table: Arc::new(Mutex::new(HashMap::new())),
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
