use core::fmt;
use std::{os::unix::thread, time::Duration};
use log::debug;
use alloc::vec;
use spin::Mutex;
use alloc::{collections::BTreeMap, sync::Arc, vec::Vec};
use smoltcp::{iface::{Config, Interface, SocketSet}, phy::{Device, DeviceCapabilities, RxToken, TunTapInterface, TxToken}, time::Instant, wire::{EthernetAddress, EthernetFrame}};

pub const MAX_FDB_ENTRIES: usize = 1024;
pub type BridgeifPortmask = u8;
pub const BR_FLOOD: BridgeifPortmask = !0;

const MAX_FRAME_SIZE: usize = 1522; // 略大于标准以太网帧的最大大小

#[derive(Clone)]
pub struct BridgePort {
    // pub port_iface: Interface,
    pub port_config: Config,                        // 端口的配置
    pub port_now: Instant,                          // 端口的当前时间
    pub port_device: Arc<Mutex<TunTapInterface>>,   // 端口对应的设备
    pub port_num: u8,                               // 端口号
}

impl BridgePort {
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

    pub fn send(&mut self, frame: &EthernetFrame<&mut [u8]>, time: Instant) -> Result<(), ()> {
        if let Some(tx_token) = self.port_device.lock()
            .transmit(time) {
            tx_token.consume(frame.as_ref().len(), |buffer: &mut [u8]| {
                buffer[..frame.as_ref().len()].copy_from_slice(frame.as_ref());
            });
            Ok(())
        } else {
            Err(())
        }
    }

    pub fn recv(&mut self, time: Instant) -> Option<[u8; MAX_FRAME_SIZE]> {
        if let Some((rx_token, _)) = self.port_device.lock().receive(time) {
            let mut frame_buffer = [0u8; MAX_FRAME_SIZE];
            let mut frame_len = 0;
            
            rx_token.consume(&mut |buffer: &mut [u8]| {
                if buffer.len() > MAX_FRAME_SIZE {
                    debug!("Received frame too large, truncating");
                    frame_len = MAX_FRAME_SIZE;
                } else {
                    frame_len = buffer.len();
                }
                frame_buffer[..frame_len].copy_from_slice(&buffer[..frame_len]);
            });

            if frame_len > 0 {
                match EthernetFrame::new_checked(&frame_buffer[..frame_len]) {
                    Ok(_) => {
                        debug!("Received valid frame on port {}", self.port_num);
                        Some(frame_buffer)
                    }
                    Err(_) => {
                        debug!("Received invalid frame on port {}", self.port_num);
                        None
                    }
                }
            } else {
                None
            }
        } else {
            None
        }
    }

    pub fn capabilities(&self) -> DeviceCapabilities {
        self.port_device.lock()
            .capabilities()
    }

    pub fn get_port_num(&self) -> Option<u8> {
        Some(self.port_num)
    }

    pub fn create_interface(&mut self, now: Instant) -> Interface {
        let port_config = self.port_config.clone();

        self.with_device_mut(|device| {
            Interface::new(port_config, device, now)
        })
    }

    pub fn with_device<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&TunTapInterface) -> R
    {
        let device = self.port_device.lock();
        f(&device)
    }

    pub fn with_device_mut<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut TunTapInterface) -> R
    {
        let mut device = self.port_device.lock();
        f(&mut device)
    }

    pub fn get_port_device(&mut self) -> Arc<Mutex<TunTapInterface>> {
        self.port_device.clone()
    }

    pub fn get_config_mut(&mut self) -> &mut Config {
        &mut self.port_config
    }

    pub fn get_config(&self) -> &Config {
        &self.port_config
    }

    pub fn port_config(&self) -> &Config {
            &self.port_config
        }

    pub fn add_config(&mut self, config: Config) {
        self.port_config = config;
    }

    pub fn get_instant(&self) -> Instant {
        self.port_now
    }
}

#[derive(Debug, Clone, Copy)]
pub struct BridgeSfdbEntry {
    pub used: bool,                 // 表示该项是否已使用
    pub dst_ports: usize,           // 目标端口的位掩码
}

#[derive(Debug, Clone)]
pub struct BridgeSfdb {
    pub max_fdb_entries: u16,       // 最大表项数
    pub fdb: Arc<Mutex<BTreeMap<EthernetAddress, BridgeSfdbEntry>>>,   // 指向表项数组的指针
}

impl BridgeSfdb {
    pub fn new(max_entries: u16) -> Self {
        BridgeSfdb {
            max_fdb_entries: max_entries,
            fdb: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    pub fn add_entry(&self, addr: EthernetAddress, ports: usize) -> Result<(), &'static str> {
        let mut fdb = self.fdb.lock();

        if fdb.len() >= self.max_fdb_entries as usize {
            return Err("FDB is full");
        }

        fdb.insert(addr, BridgeSfdbEntry {
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

    pub fn get_entry(&self, addr: &EthernetAddress) -> Option<BridgeSfdbEntry> {
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

    pub fn sfdb_add(&self, addr: &EthernetAddress, ports: usize) -> Result<(), &'static str> {
        let mut fdb = self.fdb.lock();

        if fdb.len() >= self.max_fdb_entries as usize {
            // 如果 FDB 已满，尝试找到一个未使用的条目并替换它
            if let Some(unused_key) = fdb.iter().find(|(_, v)| !v.used).map(|(k, _)| *k) {
                fdb.remove(&unused_key);
            } else {
                return Err("FDB is full");
            }
        }

        fdb.insert(*addr, BridgeSfdbEntry {
            used: true,
            dst_ports: ports,
        });

        Ok(())
    }

    pub fn sfdb_remove(&self, addr: &EthernetAddress) -> Result<(), &'static str> {
        let mut fdb = self.fdb.lock();

        if let Some(unused_key) = fdb.iter().find(|(k, v)| !v.used && *k == addr).map(|(k, _)| *k) {
            fdb.remove(&unused_key);
        } else {
            return Err("FDB is full");
        }

        Ok(())
    }

    pub fn get(&self, addr: &EthernetAddress) -> Option<BridgeSfdbEntry> {
        let fdb = self.fdb.lock();
        fdb.get(addr).cloned()
    }
}

pub struct Bridge {
    // pub config: Config,                         // 网桥自己的 Config
    // pub device: Arc<Mutex<BridgeDevice>>,       // 网桥自己的设备
    pub ethaddr: EthernetAddress,               // 网桥的 MAC 地址
    pub max_ports: u8,                          // 端口的最大数量
    pub num_ports: u8,                          // 端口的当前数量
    pub ports: BTreeMap<u8, BridgePort>,        // 端口列表
    pub fdb_static: BridgeSfdb,
    // pub fdb_dynamic: BridgeDfdb,
}

impl Bridge {
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
                    let mut fdb = self.fdb_static.fdb.lock();
                    fdb.entry(src_mac).or_insert(BridgeSfdbEntry {
                        used: true,
                        dst_ports: 1 << port_num
                    });
                }
            }
        }

        println!("Bridge: process_frames over");
    }

    pub fn decide_forward_ports(&self, dst_addr: &EthernetAddress, in_port: u8) -> Vec<u8> {
        if let Some(entry) = self.fdb_static.get_entry(dst_addr) {
            debug!("Static FDB {}", entry.dst_ports);
            return vec![entry.dst_ports as u8];
        }

        // if let Some(port) = self.fdb_dynamic.get_entry(dst_addr) {
        //     debug!("Dynamic FDB {}", port);
        //     return vec![port];
        // }

        debug!("Broadcasting frame");
        (0..self.num_ports).filter(|&p| p != in_port).collect()
    }

    pub fn fdb_add(&self, addr: &EthernetAddress, ports: usize) -> Result<(), &'static str> {
        self.fdb_static.sfdb_add(addr, ports)
    }

    pub fn fdb_remove(&self, addr: &EthernetAddress) -> Result<(), &'static str> {
        self.fdb_static.sfdb_remove(addr)
    }

    pub fn find_dst_ports(&self, dst_addr: &EthernetAddress) -> BridgeifPortmask {
        let fdb = self.fdb_static.fdb.lock();

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

    pub fn remove_port(&mut self, port_num: u8) -> Option<BridgePort> {
        if self.ports.remove(&port_num).is_some() {
            self.num_ports -= 1;
        }
        self.ports.remove(&port_num)
    }
}

#[derive(Clone)]
pub struct BridgeWrapper(Arc<Mutex<Bridge>>);

unsafe impl Send for BridgeWrapper {}
unsafe impl Sync for BridgeWrapper {}

impl fmt::Debug for BridgeWrapper {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BridgeWrapper")
            .field("is_some", &true)
            .finish()
    }
}

impl BridgeWrapper {
    pub fn new<D: Device + 'static>(_config: Config, _device: D, ethaddr: EthernetAddress, max_ports: u8, _ts: Instant) -> Self {
        // let _device = Arc::new(Mutex::new(TunTapInterface::new(device)));
        BridgeWrapper(Arc::new(Mutex::new(Bridge {
            // config,
            // device,
            ethaddr,
            max_ports,
            num_ports: 0,
            ports: BTreeMap::new(),
            fdb_static: BridgeSfdb::new(MAX_FDB_ENTRIES as u16),
            // fdb_dynamic: BridgeDfdb::new(MAX_FDB_ENTRIES as u16, ts),
        })))
    }

    pub fn get_bridge(&self) -> Arc<Mutex<Bridge>> {
        self.0.clone()
    }

    pub fn get_bridgeport(&self, port_num: u8) -> Option<BridgePort> {
        let bridge = self.0.lock();
        bridge.ports.get(&port_num).cloned()
    }

    pub fn num_ports(&self) -> u8 {
        let bridge = self.0.lock();
        bridge.num_ports
    }

    pub fn add_port(&self, port_config: Config, port_device: TunTapInterface, port_num: u8, port_now: Instant) -> Result<(), &'static str> {
        let mut bridge = self.0.lock();
        if bridge.num_ports >= bridge.max_ports {
            return Err("Maximum number of ports reached");
        }

        let port_device = Arc::new(Mutex::new(port_device));
    
        let port = BridgePort {
                    // port_iface,
                    port_now,
                    port_config,
                    port_device: port_device.clone(),
                    port_num,
                };
    
        bridge.ports.insert(port_num, port);
        bridge.num_ports += 1;
        Ok(())
    }

    pub fn process_frame(&self, frame: &EthernetFrame<&[u8]>, in_port: u8, time: Instant) -> Result<(), &'static str> {
        let bridge = self.0.lock();
        let src_addr = frame.src_addr();
        let dst_addr = frame.dst_addr();

        debug!("src_addr {} dst_addr {}", src_addr, dst_addr);
        debug!("Received frame from port {}", in_port);

        // if bridge.fdb_dynamic.update_entry(src_addr, in_port).is_err() {
        //     debug!("Failed to update dynamic FDB for source address");
        //     // 可以根据需求决定是否继续处理，还是返回错误
        // }
    
        // 决定转发端口
        let out_ports = bridge.decide_forward_ports(&dst_addr, in_port);
        debug!("transport port {:?}", out_ports);
    
        // 在转发帧之前，释放对网桥的锁
        drop(bridge);

        for &port_num in &out_ports {
            if let Some(port) = self.0.lock().ports.get_mut(&(port_num)) {
                self.forward_frame(frame, port, time)?;
            } else {
                debug!("PPort {} not found", port_num);
                continue; // 或者选择返回错误
            }
        }

        Ok(())
    }

    fn forward_frame(&self, frame: &EthernetFrame<&[u8]>, port: &mut BridgePort, time: Instant) -> Result<(), &'static str> {
        let mut binding = port.port_device.lock();
        let tx_token = binding.transmit(time).ok_or("Failed to acquire transmit token")?;

        tx_token.consume(frame.as_ref().len(), |buffer: &mut [u8]| {
            buffer.copy_from_slice(frame.as_ref());
            debug!("buffer {:?}", buffer);
        });

        Ok(())
    }

    pub fn receive_frame(&self, time: Instant) -> Option<(u8, Vec<u8>)> {
        let mut bridge = self.0.lock();
        for (port_num, port) in bridge.ports.iter_mut() {
            println!("Bridge: port_num {:?}", port_num);
            let data = port.recv(time);
            if let Some(data) = data {
                return Some((*port_num, data.to_vec()));
            }
        }
        None
    }

    pub fn fdb_add(&mut self, addr: &EthernetAddress, ports: usize) -> Result<(), &'static str> {
        let bridge = self.0.lock();
        bridge.fdb_static.add_entry(*addr, ports)
    }

    pub fn fdb_remove(&mut self, addr: &EthernetAddress) -> Result<(), &'static str> {
        let bridge = self.0.lock();
        bridge.fdb_static.remove_entry(addr)
    }

    pub fn find_dst_ports(&self, dst_addr: &EthernetAddress) -> BridgeifPortmask {
        let bridge = self.0.lock();
        let mask  = (0..bridge.num_ports).fold(0, |acc, i| acc | (1 << i));
        if let Some(entry) = bridge.fdb_static.get_entry(dst_addr) {
            return (1 <<entry.dst_ports & mask) as u8;
        } else {
            return BR_FLOOD & mask as u8;
        }
        // bridge.fdb_dynamic.get_dst_ports(dst_addr) & (mask as u8)
    }

    pub fn get_port(&self, netif: &Interface) -> Option<u8> {
        let bridge = self.0.lock();
        for (port_num, bridge_port) in bridge.ports.iter() {
            let addr = bridge_port.get_config().hardware_addr;

            if addr == netif.hardware_addr() {
                return Some(*port_num);
            }
        }
        None
    }

    // 同步方式处理数据帧
    pub fn process_one_frame(&self) {
        let mut bridge = self.0.lock();
        bridge.process_frames();
    }

    // 直接运行
    pub fn run(&self, interval: Duration) {
            self.process_one_frame();
            std::thread::sleep(interval);
    }
}
