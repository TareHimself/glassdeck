// Thin adapter from CEF's C++ API to a C ABI for Rust. Rust owns all D3D11 work.
#include <windows.h>

#include <cstdint>
#include <future>
#include <mutex>
#include <string>

#include "include/cef_app.h"
#include "include/cef_browser.h"
#include "include/cef_client.h"
#include "include/cef_render_handler.h"

extern "C" {
typedef void (*gd_web_frame_fn)(void* user, void* shared_handle, uint32_t width, uint32_t height, uint64_t timestamp_us);
}

namespace {

class Client : public CefClient, public CefRenderHandler, public CefLifeSpanHandler {
 public:
  Client(int width, int height, gd_web_frame_fn on_frame, void* user)
      : width_(width), height_(height), on_frame_(on_frame), user_(user) {}

  CefRefPtr<CefRenderHandler> GetRenderHandler() override { return this; }
  CefRefPtr<CefLifeSpanHandler> GetLifeSpanHandler() override { return this; }

  void GetViewRect(CefRefPtr<CefBrowser>, CefRect& rect) override { rect = CefRect(0, 0, width_, height_); }

  void OnPaint(CefRefPtr<CefBrowser>, PaintElementType, const RectList&, const void*, int, int) override {}

  // The handle is only valid during this call; Rust copies the texture out before returning.
  void OnAcceleratedPaint(CefRefPtr<CefBrowser>, PaintElementType type, const RectList&,
                          const CefAcceleratedPaintInfo& info) override {
    if (type != PET_VIEW) return;
    on_frame_(user_, info.shared_texture_handle, static_cast<uint32_t>(info.extra.visible_rect.width),
              static_cast<uint32_t>(info.extra.visible_rect.height), info.extra.timestamp);
  }

  void OnAfterCreated(CefRefPtr<CefBrowser> browser) override {
    std::lock_guard<std::mutex> lock(mutex_);
    browser_ = browser;
  }

  void OnBeforeClose(CefRefPtr<CefBrowser>) override {
    std::lock_guard<std::mutex> lock(mutex_);
    browser_ = nullptr;
    closed_.set_value();
  }

  // Blocks until the browser is gone, so CefShutdown can follow.
  void CloseAndWait() {
    CefRefPtr<CefBrowser> browser;
    {
      std::lock_guard<std::mutex> lock(mutex_);
      browser = browser_;
    }
    if (!browser) return;
    std::future<void> closed = closed_.get_future();
    browser->GetHost()->CloseBrowser(true);
    closed.wait();
  }

 private:
  int width_;
  int height_;
  gd_web_frame_fn on_frame_;
  void* user_;
  std::mutex mutex_;
  CefRefPtr<CefBrowser> browser_;
  std::promise<void> closed_;

  IMPLEMENT_REFCOUNTING(Client);
};

class App : public CefApp {
 public:
  void OnBeforeCommandLineProcessing(const CefString&, CefRefPtr<CefCommandLine> command_line) override {
    // Wallpapers play sound and video without anyone clicking first.
    command_line->AppendSwitchWithValue("autoplay-policy", "no-user-gesture-required");
  }

 private:
  IMPLEMENT_REFCOUNTING(App);
};

CefRefPtr<Client> g_client;

}  // namespace

extern "C" {

// Chromium relaunches this executable for its helper processes; returns their exit code, or -1 in the main process.
int gd_web_subprocess(void) {
  CefMainArgs args(GetModuleHandleW(nullptr));
  CefRefPtr<App> app(new App);
  return CefExecuteProcess(args, app, nullptr);
}

// Starts CEF on its own message-loop thread and opens one windowless browser. Returns 0 on success.
int gd_web_start(const char* url, uint32_t width, uint32_t height, uint32_t fps, const char* cache_dir,
                 gd_web_frame_fn on_frame, void* user) {
  CefMainArgs args(GetModuleHandleW(nullptr));
  CefSettings settings;
  settings.no_sandbox = true;
  settings.windowless_rendering_enabled = true;
  settings.multi_threaded_message_loop = true;
  settings.log_severity = LOGSEVERITY_WARNING;
  CefString(&settings.root_cache_path).FromString(cache_dir);
  CefRefPtr<App> app(new App);
  if (!CefInitialize(args, settings, app, nullptr)) return 1;

  g_client = new Client(static_cast<int>(width), static_cast<int>(height), on_frame, user);
  CefWindowInfo window;
  window.SetAsWindowless(nullptr);
  window.shared_texture_enabled = true;
  CefBrowserSettings browser_settings;
  browser_settings.windowless_frame_rate = static_cast<int>(fps);
  if (!CefBrowserHost::CreateBrowser(window, g_client, std::string(url), browser_settings, nullptr, nullptr)) {
    g_client = nullptr;
    CefShutdown();
    return 2;
  }
  return 0;
}

// Closes the browser and shuts CEF down; call from the thread that called gd_web_start.
void gd_web_stop(void) {
  if (g_client) {
    g_client->CloseAndWait();
    g_client = nullptr;
  }
  CefShutdown();
}

}  // extern "C"
