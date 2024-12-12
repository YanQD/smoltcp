use std::os::unix::io::AsRawFd;
use smoltcp::{phy::{Device, DeviceCapabilities, Medium, RxToken, TunTapInterface, TxToken}, time::Instant};
// 首先定义 FrameCapture 结构体和实现
pub struct FrameCapture {
    inner: TunTapInterface,
}

impl FrameCapture {
    pub fn new(name: &str, medium: Medium) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(FrameCapture {
            inner: TunTapInterface::new(name, medium)?,
        })
    }

    pub fn as_raw_fd(&self) -> i32 {
        self.inner.as_raw_fd()
    }
}

// 实现 Device trait
impl Device for FrameCapture {
    type RxToken<'a> = FrameCaptureRxToken<'a>; 
    type TxToken<'a> = FrameCaptureTxToken<'a>;

    fn capabilities(&self) -> DeviceCapabilities {
        self.inner.capabilities()
    }

    fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        self.inner.receive(_timestamp).map(|(rx, tx)| {
            // 包装接收和发送令牌
            (
                FrameCaptureRxToken { inner: rx },
                FrameCaptureTxToken { inner: tx }
            )
        })
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        self.inner.transmit(_timestamp).map(|tx| {
            FrameCaptureTxToken { inner: tx }
        })
    }
}

// 包装接收令牌
pub struct FrameCaptureRxToken<'a> {
    inner: <TunTapInterface as Device>::RxToken<'a>,
}

impl<'a> RxToken for FrameCaptureRxToken<'a> {
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        self.inner.consume(|buffer| {
            // 打印接收到的帧内容
            println!("Received frame:");
            print_frame(buffer);
            f(buffer)
        })
    }
}

// 包装发送令牌
pub struct FrameCaptureTxToken<'a> {
    inner: <TunTapInterface as Device>::TxToken<'a>,
}

impl<'a> TxToken for FrameCaptureTxToken<'a> {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        self.inner.consume(len, |buffer| {
            let result = f(buffer);
            // 打印发送的帧内容
            println!("Transmitting frame:");
            print_frame(buffer);
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