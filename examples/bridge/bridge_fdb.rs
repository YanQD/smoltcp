// use smoltcp::wire::EthernetAddress;
// use spin::Mutex;
// use log::debug;
// use smoltcp::time::Instant;
// use smoltcp::time::Duration;
// use alloc::{collections::{btree_map, BTreeMap}, sync::Arc};

// const BR_FDB_TIMEOUT_SEC: Duration = Duration::from_secs(60 * 5);   //5 minutes FDB timeout
pub const MAX_FDB_ENTRIES: usize = 1024;
pub type BridgeifPortmask = u8;
pub const BR_FLOOD: BridgeifPortmask = !0;

// #[derive(Debug, Clone, Copy)]
// pub struct BridgeDfdbEntry {
//     used: bool,             // 表项是否使用的标志
//     port: u8,               // 表项对应的端口号
//     ts: Instant,            // 表项的时间戳
// }

// #[derive(Debug, Clone)]
// pub struct BridgeDfdb {
//     pub max_fdb_entries: u16,                   // 最大表项数
//     pub fdb: Arc<Mutex<BTreeMap<EthernetAddress, BridgeDfdbEntry>>>,      // 指向表项数组的指针
// }

// impl BridgeDfdb {
//     // 初始化 FDB
//     pub fn new(max_entries: u16, ts: Instant) -> Self {
//         let mut fdb = BTreeMap::new();
//         for _ in 0..max_entries {
//             let dfdb = BridgeDfdbEntry {
//                 used: false,
//                 port: 0,
//                 ts,
//             };
//             fdb.insert(EthernetAddress([0; 6]), dfdb) ;
//         }
//         BridgeDfdb {
//             max_fdb_entries: max_entries,
//             fdb: Arc::new(Mutex::new(fdb)),
//         }
//     }

//     // 更新FDB
//     #[cfg(feature = "std")]
//     pub fn update_src(&self, src_addr: &EthernetAddress, port_idx: u8) {
//         let mut fdb = self.fdb.lock();

//         // 首先检查容量
//         if !fdb.contains_key(src_addr) && fdb.len() >= self.max_fdb_entries as usize {
//             debug!("FDB is full, flooding may occur");
//             return;
//         }
        
//         match fdb.entry(*src_addr) {
//             btree_map::Entry::Occupied(mut entry) => {
//                 let entry = entry.get_mut();
//                 if entry.ts.elapsed() < BR_FDB_TIMEOUT_SEC {
//                     debug!("br: update src {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} (from {}) @ existing entry",
//                             src_addr.0[0], src_addr.0[1], src_addr.0[2], src_addr.0[3], src_addr.0[4], src_addr.0[5],
//                             port_idx);
//                     entry.ts = Instant::now();
//                     entry.port = port_idx;
//                 } else {
//                     // Entry has expired, update it
//                     *entry = BridgeDfdbEntry {
//                         used: true,
//                         port: port_idx,
//                         ts: Instant::now(),
//                     };
//                     debug!("br: update src {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} (from {}) @ expired entry",
//                             src_addr.0[0], src_addr.0[1], src_addr.0[2], src_addr.0[3], src_addr.0[4], src_addr.0[5],
//                             port_idx);
//                 }
//             },
//             btree_map::Entry::Vacant(entry) => {
//                 entry.insert(BridgeDfdbEntry {
//                     used: true,
//                     port: port_idx,
//                     ts: Instant::now(),
//                 });
//                 debug!("br: create src {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} (from {}) @ new entry",
//                         src_addr.0[0], src_addr.0[1], src_addr.0[2], src_addr.0[3], src_addr.0[4], src_addr.0[5],
//                         port_idx);
//             }
//         }
//     }

//     // 用于根据目的 MAC 地址查找应该转发到哪个端口
//     #[cfg(feature = "std")]
//     pub fn get_dst_ports(&self, dst_addr: &EthernetAddress) -> BridgeifPortmask {
//         let fdb = self.fdb.lock();

//         debug!("dfdb {:02x?}", fdb);

//         fdb.get(dst_addr).map_or(255, |entry| {
//             if entry.used && entry.ts.elapsed() < BR_FDB_TIMEOUT_SEC {
//                 debug!("get_dst_ports {}", 1 << entry.port);
//                 1 << entry.port
//             } else {
//                 BR_FLOOD
//             }
//         })
//     }

//     // // FDB 的老化函数
//     // pub fn age_one_second(&self, now: Instant) {
//     //     let mut fdb = self.fdb.lock();

//     //     // 使用 retain 方法来删除过期的条目
//     //     fdb.retain(|addr, entry| {
//     //         if now.duration_since(entry.ts) < BR_FDB_TIMEOUT_SEC {
//     //             true // 保留未过期的条目
//     //         } else {
//     //             debug!("br: age out {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
//     //                     addr.0[0], addr.0[1], addr.0[2],
//     //                     addr.0[3], addr.0[4], addr.0[5]);
//     //             false // 删除过期的条目
//     //         }
//     //     });
//     // }

//     pub fn update_entry(&self, addr: EthernetAddress, port: u8) -> Result<(), &'static str> {
//         let mut fdb = self.fdb.lock();

//         if fdb.len() >= self.max_fdb_entries as usize && !fdb.contains_key(&addr) {
//             return Err("FDB is full");
//         }

//         fdb.insert(addr, BridgeDfdbEntry {
//             used: true,
//             port,
//             ts: Instant::now(),
//         });

//         Ok(())
//     }

//     pub fn get_entry(&self, addr: &EthernetAddress) -> Option<u8> {
//         let fdb = self.fdb.lock();
//         fdb.get(addr).filter(|entry| entry.used).map(|entry| entry.port)
//     }

//     // pub fn age_entries(&self, now: Instant) {
//     //     let mut fdb = self.fdb.lock();

//     //     fdb.retain(|_, entry| {
//     //         if now.duration_since(entry.ts) < BR_FDB_TIMEOUT_SEC {
//     //             true
//     //         } else {
//     //             entry.used = false;
//     //             false
//     //         }
//     //     });
//     // }

//     pub fn clear(&self) {
//         let mut fdb = self.fdb.lock();
//         fdb.clear();
//     }

//     pub fn len(&self) -> usize {
//         let fdb = self.fdb.lock();
//         fdb.len()
//     }

//     pub fn is_full(&self) -> bool {
//         let fdb = self.fdb.lock();
//         fdb.len() >= self.max_fdb_entries as usize
//     }
// }