//! Diagnostic: does Windows Graphics Capture let an unpackaged Win32 app turn off
//! the yellow capture border on this machine?
//!
//! The probe creates and captures ONLY its own window. It never touches any other
//! window, game, or user application.

use std::time::{Duration, Instant};

use windows::core::PCWSTR;
use windows::core::{factory, Interface, HSTRING};
use windows::Foundation::Metadata::ApiInformation;
use windows::Graphics::Capture::{
    Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Win32::Foundation::{E_FAIL, HMODULE, HWND};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::System::WinRT::Direct3D11::CreateDirect3D11DeviceFromDXGIDevice;
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, DispatchMessageW, PeekMessageW, ShowWindow, TranslateMessage,
    CW_USEDEFAULT, MSG, PM_REMOVE, SW_SHOW, WINDOW_EX_STYLE, WS_OVERLAPPEDWINDOW,
};

fn main() {
    println!("border_probe: Windows build check");

    // SAFETY: called once for the process; never paired with RoUninitialize.
    if let Err(e) = unsafe { RoInitialize(RO_INIT_MULTITHREADED) } {
        println!("RoInitialize = Err {:#010x} {}", e.code().0, e.message());
    }

    let hwnd = match create_window() {
        Ok(h) => h,
        Err(e) => {
            println!("window creation = Err {:#010x} {}", e.code().0, e.message());
            return;
        }
    };
    pump(Duration::from_millis(200));

    let supported = match GraphicsCaptureSession::IsSupported() {
        Ok(b) => {
            println!("IsSupported = {b}");
            b
        }
        Err(e) => {
            println!("IsSupported = Err {:#010x} {}", e.code().0, e.message());
            false
        }
    };
    if !supported {
        destroy_window(hwnd);
        println!("border_probe: done");
        return;
    }

    match ApiInformation::IsPropertyPresent(
        &HSTRING::from("Windows.Graphics.Capture.GraphicsCaptureSession"),
        &HSTRING::from("IsBorderRequired"),
    ) {
        Ok(b) => println!("IsBorderRequired property present = {b}"),
        Err(e) => println!(
            "IsBorderRequired property present = Err {:#010x} {}",
            e.code().0,
            e.message()
        ),
    }

    let (device, _context) = match create_device() {
        Ok(v) => v,
        Err(e) => {
            println!(
                "D3D11CreateDevice = Err {:#010x} {}",
                e.code().0,
                e.message()
            );
            destroy_window(hwnd);
            std::process::exit(1);
        }
    };
    let d3d = match wrap_device(&device) {
        Ok(d) => d,
        Err(e) => {
            println!(
                "CreateDirect3D11DeviceFromDXGIDevice = Err {:#010x} {}",
                e.code().0,
                e.message()
            );
            destroy_window(hwnd);
            std::process::exit(1);
        }
    };

    let item = match create_item(hwnd) {
        Ok(i) => i,
        Err(e) => {
            println!("CreateForWindow = Err {:#010x} {}", e.code().0, e.message());
            destroy_window(hwnd);
            println!("border_probe: done");
            return;
        }
    };

    let size = match item.Size() {
        Ok(s) => s,
        Err(e) => {
            println!(
                "GraphicsCaptureItem::Size = Err {:#010x} {}",
                e.code().0,
                e.message()
            );
            destroy_window(hwnd);
            println!("border_probe: done");
            return;
        }
    };

    let pool = match Direct3D11CaptureFramePool::CreateFreeThreaded(
        &d3d,
        DirectXPixelFormat::B8G8R8A8UIntNormalized,
        2,
        size,
    ) {
        Ok(p) => p,
        Err(e) => {
            println!(
                "CreateFreeThreaded = Err {:#010x} {}",
                e.code().0,
                e.message()
            );
            destroy_window(hwnd);
            println!("border_probe: done");
            return;
        }
    };

    let session = match pool.CreateCaptureSession(&item) {
        Ok(s) => s,
        Err(e) => {
            println!(
                "CreateCaptureSession = Err {:#010x} {}",
                e.code().0,
                e.message()
            );
            destroy_window(hwnd);
            println!("border_probe: done");
            return;
        }
    };

    match session.SetIsBorderRequired(false) {
        Ok(()) => println!("SetIsBorderRequired(false) = Ok"),
        Err(e) => println!(
            "SetIsBorderRequired(false) = Err {:#010x} {}",
            e.code().0,
            e.message()
        ),
    }

    match session.StartCapture() {
        Ok(()) => println!("StartCapture = Ok"),
        Err(e) => println!("StartCapture = Err {:#010x} {}", e.code().0, e.message()),
    }

    pump(Duration::from_secs(5));

    if let Err(e) = session.Close() {
        println!("session.Close = Err {:#010x} {}", e.code().0, e.message());
    }
    if let Err(e) = pool.Close() {
        println!("pool.Close = Err {:#010x} {}", e.code().0, e.message());
    }

    destroy_window(hwnd);
    println!("border_probe: done");
}

/// Registers the probe's own window class and shows a 480x320 window.
fn create_window() -> windows::core::Result<HWND> {
    // Predefined STATIC class: a plain window with no custom class registration.
    let class_name: Vec<u16> = "STATIC\0".encode_utf16().collect();
    let title: Vec<u16> = "ClipCheck border probe\0".encode_utf16().collect();

    // SAFETY: class name and title buffers are NUL-terminated and outlive the call.
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR(class_name.as_ptr()),
            PCWSTR(title.as_ptr()),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            480,
            320,
            None,
            None,
            None,
            None,
        )
    }?;

    // SAFETY: hwnd is the valid window we just created.
    let _ = unsafe { ShowWindow(hwnd, SW_SHOW) };
    Ok(hwnd)
}

/// Pumps this thread's window messages for the given duration.
fn pump(duration: Duration) {
    let end = Instant::now() + duration;
    loop {
        let mut msg = MSG::default();
        // SAFETY: msg is a valid out-pointer; no HWND filter is used.
        while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {
            // SAFETY: msg was filled in by PeekMessageW.
            unsafe {
                let _ = TranslateMessage(&msg);
                let _ = DispatchMessageW(&msg);
            }
        }
        if Instant::now() >= end {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn destroy_window(hwnd: HWND) {
    // SAFETY: hwnd is the probe's own window, destroyed exactly once.
    if let Err(e) = unsafe { DestroyWindow(hwnd) } {
        println!("DestroyWindow = Err {:#010x} {}", e.code().0, e.message());
    }
}

/// Same D3D11 device pattern as crates/capture/src/wgc.rs `create_device`.
fn create_device() -> windows::core::Result<(ID3D11Device, ID3D11DeviceContext)> {
    let mut device: Option<ID3D11Device> = None;
    let mut context: Option<ID3D11DeviceContext> = None;
    // SAFETY: out-pointers reference live Option slots; the API writes valid
    // COM references into them on success.
    unsafe {
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device as *mut _),
            None,
            Some(&mut context as *mut _),
        )?;
    }
    match (device, context) {
        (Some(d), Some(c)) => Ok((d, c)),
        _ => Err(windows::core::Error::from(E_FAIL)),
    }
}

/// Wraps the D3D11 device as the WinRT IDirect3DDevice (same steps as wgc.rs).
fn wrap_device(device: &ID3D11Device) -> windows::core::Result<IDirect3DDevice> {
    let dxgi: IDXGIDevice = device.cast()?;
    // SAFETY: dxgi is a valid IDXGIDevice owned for the duration of the call.
    let inspectable = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi) }?;
    inspectable.cast()
}

/// Creates a GraphicsCaptureItem for the probe's own window only.
fn create_item(hwnd: HWND) -> windows::core::Result<GraphicsCaptureItem> {
    let interop = factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
    // SAFETY: hwnd is the probe's own top-level window.
    unsafe { interop.CreateForWindow(hwnd) }
}
