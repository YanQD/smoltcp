use std::thread;

use alloc::vec;
use log::debug;
use spin::Mutex;
use crate::frame::{CapturedFrame, FrameCapture};
use alloc::{collections::BTreeMap, sync::Arc, vec::Vec};
use smoltcp::{
    time::Instant, wire::EthernetAddress,
    iface::{Interface, SocketHandle, SocketSet}, 
    phy::{wait as phy_wait, Device, DeviceCapabilities, RxToken, TxToken}
};

pub type BridgeifPortmask = u8;
pub const MAX_FRAME_SIZE: usize = 1522; // 略大于标准以太网帧的最大大小
pub const MAX_FDB_ENTRIES: usize = 10;
pub const BR_FLOOD: BridgeifPortmask = !0;

pub struct SwitchPort {
    pub port_iface: Interface,                      // 端口对应的接口
    pub port_device: Arc<Mutex<FrameCapture>>,      // 端口对应的设备
    pub port_num: u8,                               // 端口号
}

impl SwitchPort {
    // 创建一个新的 SwitchPort
    pub fn new(port_iface: Interface, device: FrameCapture, port_num: u8) -> Self {
        let port_device = Arc::new(Mutex::new(device));
        SwitchPort {
            port_iface,
            port_device,
            port_num,
        }
    }

    // 接收数据帧
    pub fn receive_frame(&mut self, timestamp: Instant) -> Option<Vec<u8>> {
        let mut device = self.port_device.lock();
        
        println!("Trying to receive on port...");

        // 使用 TunTapInterface 的 receive 方法
        if let Some((rx_token, _tx_token)) = device.receive(timestamp) {
            // 使用 RxToken 来获取数据
            println!("Received data!");
            let frame_data = rx_token.consume(|buffer| {
                let mut data = Vec::new();
                data.extend_from_slice(buffer);
                data
            });
            Some(frame_data)
        } else {
            None
        }
    }

    // 发送数据帧
    pub fn transmit_frame(&mut self, timestamp: Instant, frame: &[u8]) {
        let mut device = self.port_device.lock();
        if let Some(tx_token) = device.transmit(timestamp) {
            tx_token.consume(frame.len(), |buffer| {
                buffer.copy_from_slice(frame);
            });
        }
    }

    pub fn get_frames(&self) -> CapturedFrame {
        let device = self.port_device.lock();
        device.get_frames()
    }

    pub fn capabilities(&self) -> DeviceCapabilities {
        self.port_device.lock()
            .capabilities()
    }

    pub fn get_port_num(&self) -> Option<u8> {
        Some(self.port_num)
    }

    pub fn with_device<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&FrameCapture) -> R
    {
        let device = self.port_device.lock();
        f(&device)
    }

    pub fn with_device_mut<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut FrameCapture) -> R
    {
        let mut device = self.port_device.lock();
        f(&mut device)
    }

    pub fn get_port_device(&mut self) -> Arc<Mutex<FrameCapture>> {
        self.port_device.clone()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SwicthFdbEntry {
    pub used: bool,                 // 表示该项是否已使用
    pub dst_ports: usize,           // 目标端口的位掩码
}

#[derive(Debug, Clone)]
pub struct SwicthFdb {
    pub max_fdb_entries: u16,       // 最大表项数
    pub fdb: Arc<Mutex<BTreeMap<EthernetAddress, SwicthFdbEntry>>>,   // 指向表项数组的指针
}

impl SwicthFdb {
    pub fn new(max_entries: u16) -> Self {
        SwicthFdb {
            max_fdb_entries: max_entries,
            fdb: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    pub fn add_entry(&self, addr: EthernetAddress, ports: usize) -> Result<(), &'static str> {
        let mut fdb = self.fdb.lock();

        if fdb.len() >= self.max_fdb_entries as usize {
            return Err("FDB is full");
        }

        fdb.insert(addr, SwicthFdbEntry {
            used: true,
            dst_ports: ports,
        });

        Ok(())
    }

    pub fn remove_entry(&self, addr: &EthernetAddress) -> Result<(), &'static str> {
        let mut fdb = self.fdb.lock();

        if fdb.remove(addr).is_none() {
            return Err("Entry not found");
        }

        Ok(())
    }

    pub fn get_entry(&self, addr: &EthernetAddress) -> Option<SwicthFdbEntry> {
        let fdb = self.fdb.lock();
        debug!("sfdb {:?}", fdb);
        fdb.get(addr).cloned()
    }

    pub fn update_entry(&self, addr: EthernetAddress, new_ports: usize) -> Result<(), &'static str> {
        let mut fdb = self.fdb.lock();

        if let Some(entry) = fdb.get_mut(&addr) {
            entry.dst_ports = new_ports;
            Ok(())
        } else {
            Err("Entry not found")
        }
    }

    pub fn clear(&self) {
        let mut fdb = self.fdb.lock();
        fdb.clear();
    }

    pub fn is_full(&self) -> bool {
        let fdb = self.fdb.lock();
        fdb.len() >= self.max_fdb_entries as usize
    }

    pub fn fdb_add(&self, addr: &EthernetAddress, ports: usize) -> Result<(), &'static str> {
        let mut fdb = self.fdb.lock();

        if fdb.len() >= self.max_fdb_entries as usize {
            // 如果 FDB 已满，尝试找到一个未使用的条目并替换它
            if let Some(unused_key) = fdb.iter().find(|(_, v)| !v.used).map(|(k, _)| *k) {
                fdb.remove(&unused_key);
            } else {
                return Err("FDB is full");
            }
        }

        fdb.insert(*addr, SwicthFdbEntry {
            used: true,
            dst_ports: ports,
        });

        Ok(())
    }

    pub fn fdb_remove(&self, addr: &EthernetAddress) -> Result<(), &'static str> {
        let mut fdb = self.fdb.lock();

        if let Some(unused_key) = fdb.iter().find(|(k, v)| !v.used && *k == addr).map(|(k, _)| *k) {
            fdb.remove(&unused_key);
        } else {
            return Err("FDB is full");
        }

        Ok(())
    }

    pub fn get(&self, addr: &EthernetAddress) -> Option<SwicthFdbEntry> {
        let fdb = self.fdb.lock();
        fdb.get(addr).cloned()
    }
}

pub struct Switch {
    pub ethaddr: EthernetAddress,               // 网桥的 MAC 地址
    pub max_ports: u8,                          // 端口的最大数量
    pub num_ports: u8,                          // 端口的当前数量
    pub ports: BTreeMap<u8, SwitchPort>,        // 端口列表
    pub fdb: SwicthFdb,
}

unsafe impl Send for Switch {}
unsafe impl Sync for Switch {}

impl Switch {
    pub fn new(ethaddr: EthernetAddress, max_ports: u8) -> Self {
        Switch {
            ethaddr,
            max_ports,
            num_ports: 0,
            ports: BTreeMap::new(),
            fdb: SwicthFdb::new(MAX_FDB_ENTRIES as u16),
        }
    }

    pub fn process_frames(&mut self) {
        let now = Instant::now();
        
        // 遍历所有端口接收数据
        for (port_num, port) in self.ports.iter_mut() {
            println!("Bridge: port_num {:?}", port_num);
            if let Some(frame) = port.receive_frame(now) {
                // 打印接收到的数据（使用绿色）
                println!("\x1b[32mReceived frame on port {}: {:?}\x1b[0m", port_num, frame);

                // 如果是 ARP 包（假设是以太网帧）
                if frame.len() >= 14 && frame[12] == 0x08 && frame[13] == 0x06 {
                    let dst_mac = EthernetAddress::from_bytes(&frame[0..6]);
                    let src_mac = EthernetAddress::from_bytes(&frame[6..12]);
                    
                    println!("\x1b[33mARP frame details:\x1b[0m");
                    println!("Dst MAC: {:?}", dst_mac);
                    println!("Src MAC: {:?}", src_mac);
                    
                    // 更新转发数据库
                    let mut fdb = self.fdb.fdb.lock();
                    fdb.entry(src_mac).or_insert(SwicthFdbEntry {
                        used: true,
                        dst_ports: 1 << port_num
                    });
                }
            }
        }

        println!("Bridge: process_frames over");
    }

    pub fn decide_forward_ports(&self, dst_addr: &EthernetAddress, in_port: u8) -> Vec<u8> {
        if let Some(entry) = self.fdb.get_entry(dst_addr) {
            debug!("Static FDB {}", entry.dst_ports);
            return vec![entry.dst_ports as u8];
        }

        debug!("Broadcasting frame");
        (0..self.num_ports).filter(|&p| p != in_port).collect()
    }

    pub fn fdb_add(&self, addr: &EthernetAddress, ports: usize) -> Result<(), &'static str> {
        self.fdb.fdb_add(addr, ports)
    }

    pub fn fdb_remove(&self, addr: &EthernetAddress) -> Result<(), &'static str> {
        self.fdb.fdb_remove(addr)
    }

    pub fn find_dst_ports(&self, dst_addr: &EthernetAddress) -> BridgeifPortmask {
        let fdb = self.fdb.fdb.lock();
        for (k, v) in fdb.iter() {
            if v.used && k == dst_addr {
                return v.dst_ports as u8;
            }
        }
        if dst_addr.0[0] & 1 != 0 {
            return BR_FLOOD;
        }
        0
    }

    pub fn remove_port(&mut self, port_num: u8) -> Option<SwitchPort> {
        if self.ports.remove(&port_num).is_some() {
            self.num_ports -= 1;
        }
        self.ports.remove(&port_num)
    }

    pub fn add_port(&mut self, port_iface: Interface, port_device: FrameCapture, port_num: u8) -> Result<(), &'static str> {
        if self.num_ports >= self.max_ports {
            return Err("Maximum number of ports reached");
        }
        let port = SwitchPort::new(port_iface, port_device, port_num);
        self.ports.insert(port_num, port);
        self.num_ports += 1;
        Ok(())
    }

    pub fn get_switchport(&self, port: usize) -> Option<&SwitchPort> {
        self.ports.get(&(port as u8))
    }

    pub fn poll<F>(
        &mut self, 
        timestamp: Instant, 
        sockets: &mut SocketSet<'_>,
        #[allow(unused_variables)]
        socket_handle: SocketHandle,
        num: u8,
        mut f: F)
    where
        F: FnMut(&mut SocketSet<'_>)
    {
        // 第一步：处理指定端口的网络操作
        if let Some(port) = self.ports.get_mut(&num) {
            let fd = port.port_device.lock().as_raw_fd();
            {
                let port_iface: &mut Interface = &mut port.port_iface;
                let mut device = port.port_device.lock();
                port_iface.poll(timestamp, &mut *device, sockets);
                drop(device);
            }

            f(sockets);
            
            let port_iface: &mut Interface = &mut port.port_iface;
            phy_wait(fd, port_iface.poll_delay(timestamp, &sockets)).expect("wait error");
        }

        println!("Bridge poll checkpoint1!");

        // 第二步：检查其他端口的数据
        for (port_num, port) in self.ports.iter_mut() {
            // 跳过当前处理的端口
            if port_num == &num {
                continue;
            }

            println!("Checking port {} for data", port_num);
            let mut device = port.port_device.lock();
            
            // 检查并处理接收到的数据
            if let Some((rx, _tx)) = device.receive(timestamp) {
                rx.consume(|buffer| {
                    println!("Received {} bytes on port {}", buffer.len(), port_num);
                    println!("Data: {:?}", buffer);
                });
            }
        }
    }
}
