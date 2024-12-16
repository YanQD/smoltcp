use smoltcp::time::Instant;
use std::sync::Arc;
use spin::Mutex;
use std::os::unix::io::AsRawFd;
use smoltcp::phy::{Device, DeviceCapabilities, Medium, RxToken, TunTapInterface, TxToken};

// 超时删除
#[derive(Debug, Clone)]
pub struct CapturedFrame {
    direction: Direction,
    pub timestamp: Instant,
    pub data: Vec<u8>,
}

impl CapturedFrame {
    // pub fn new() -> Self {
    //     CapturedFrame {
    //         timestamp: Instant::now(),
    //         direction: Direction::NoDefined,
    //         data: Vec::new(),
    //     }
    // }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }
}

#[derive(Debug, Clone)]
enum Direction {
    // NoDefined,
    Tx,
    Rx,
}

pub struct FrameCapture {
    inner: TunTapInterface,
    frames: Arc<Mutex<Vec<CapturedFrame>>>,
}

impl FrameCapture {
    pub fn new(name: &str, medium: Medium) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(FrameCapture {
            frames: Arc::new(Mutex::new(Vec::new())),
            inner: TunTapInterface::new(name, medium)?,
        })
    }

    // 打印所有捕获的帧
    pub fn print_captured_frames(&self) {
        let frames = &self.frames.lock();
        for (i, frame) in frames.iter().enumerate() {
            println!("\nFrame #{}", i + 1);
            println!("Timestamp: {:?}", frame.timestamp);
            println!("Direction: {:?}", frame.direction);
            println!("Data length: {}", frame.data.len());
            println!("Raw data: {:?}", frame.data);
        }
        println!("=== End of Captured Frames ===\n");
    }
    
    pub fn as_raw_fd(&self) -> i32 {
        self.inner.as_raw_fd()
    }

    pub fn get_frames(&self) -> Vec<CapturedFrame> {
        self.frames.lock().clone()
    }

    pub fn clear_frames(&self) {
        self.frames.lock().clear();
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
        let frames = Arc::clone(&self.frames);
        self.inner.receive(_timestamp).map(|(rx, tx)| {
            (
                FrameCaptureRxToken { 
                    inner: rx,
                    device_frames: Arc::clone(&frames),
                },
                FrameCaptureTxToken { 
                    inner: tx,
                    device_frames: frames,
                }
            )
        })
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        let frames = Arc::clone(&self.frames);
        self.inner.transmit(_timestamp).map(|tx| {
            FrameCaptureTxToken { 
                inner: tx,
                device_frames: frames,
            }
        })
    }
}

// 包装接收令牌
pub struct FrameCaptureRxToken<'a> {
    inner: <TunTapInterface as Device>::RxToken<'a>,
    device_frames: Arc<Mutex<Vec<CapturedFrame>>>,
}

impl<'a> RxToken for FrameCaptureRxToken<'a> {
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        self.inner.consume(|buffer| {
            // // 保存接收到的帧
            // let frame = CapturedFrame {
            //     timestamp: Instant::now(),
            //     direction: Direction::Rx,
            //     data: buffer.to_vec(),
            // };

            // // 保存发送的数据到共享的 frames 中
            // let mut frames = self.device_frames.lock();
            // frames.push(frame);

            // println!("Received frame:");
            print_frame(buffer);
            f(buffer)
        })
    }
}

// 包装发送令牌
pub struct FrameCaptureTxToken<'a> {
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
            
            // 保存发送的数据到共享的 frames 中
            let mut frames = self.device_frames.lock();
            frames.push(frame);

            println!("Transmitting frame:");
            print_frame(buffer);

            result
        })
    }
}

// 帮助函数：打印帧内容
fn print_frame(buffer: &[u8]) {
    println!("Frame length: {} bytes", buffer.len());
    println!("Raw data: {:?}", buffer);
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
