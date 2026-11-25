// Thin adapter from CEF's C++ API to a C ABI for Rust. Rust owns all D3D11 work.
#include <windows.h>

#include <algorithm>
#include <cstdint>
#include <future>
#include <mutex>
#include <string>

#include "include/cef_app.h"
#include "include/cef_browser.h"
#include "include/cef_client.h"
#include "include/cef_parser.h"
#include "include/cef_render_handler.h"
#include "include/cef_render_process_handler.h"
#include "include/cef_resource_handler.h"
#include "include/cef_scheme.h"
#include "include/cef_v8.h"

extern "C" {
typedef void (*gd_web_frame_fn)(void* user, void* shared_handle, uint32_t width, uint32_t height, uint64_t timestamp_us);

// Rust's view of an open deck: `lookup` finds an entry (0 when found), `read` reads inside one (bytes, or -1).
typedef struct {
  void* user;
  int (*lookup)(void* user, const char* path, uint64_t* offset, uint64_t* len, char* mime, uint32_t mime_cap);
  int64_t (*read)(void* user, uint64_t offset, uint64_t len, uint64_t at, void* buf, uint32_t buf_len);
} gd_deck_provider;
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

constexpr char kInitScript[] = "glassdeck_init_script";
constexpr char kDeckScheme[] = "deck";
constexpr char kDeckHost[] = "wallpaper";

// "bytes=a-b", "bytes=a-" or "bytes=-n" into [start, end); one range only.
bool ParseRange(const std::string& header, uint64_t len, uint64_t& start, uint64_t& end) {
  const std::string prefix = "bytes=";
  if (header.rfind(prefix, 0) != 0 || header.find(',') != std::string::npos) return false;
  const std::string spec = header.substr(prefix.size());
  const size_t dash = spec.find('-');
  if (dash == std::string::npos) return false;
  const std::string first = spec.substr(0, dash), last = spec.substr(dash + 1);
  try {
    if (first.empty()) {
      const uint64_t suffix = std::stoull(last);
      if (suffix == 0) return false;
      start = len - std::min(suffix, len);
      end = len;
    } else {
      start = std::stoull(first);
      end = last.empty() ? len : std::min(std::stoull(last) + 1, len);
    }
  } catch (...) {
    return false;
  }
  return start < end;
}

// Serves deck://wallpaper/<entry> straight from the entry's byte range, honouring Range for media seeking.
class DeckResourceHandler : public CefResourceHandler {
 public:
  explicit DeckResourceHandler(gd_deck_provider deck) : deck_(deck) {}

  bool Open(CefRefPtr<CefRequest> request, bool& handle_request, CefRefPtr<CefCallback>) override {
    handle_request = true;
    CefURLParts parts;
    if (!CefParseURL(request->GetURL(), parts)) {
      status_ = 400;
      return true;
    }
    const auto rules = static_cast<cef_uri_unescape_rule_t>(UU_SPACES | UU_PATH_SEPARATORS |
                                                            UU_URL_SPECIAL_CHARS_EXCEPT_PATH_SEPARATORS);
    std::string path = CefURIDecode(CefString(&parts.path), true, rules).ToString();
    if (!path.empty() && path[0] == '/') path.erase(0, 1);
    char mime[128] = {};
    if (deck_.lookup(deck_.user, path.c_str(), &offset_, &len_, mime, sizeof mime) != 0) {
      status_ = 404;
      return true;
    }
    mime_ = mime;
    start_ = 0;
    end_ = len_;
    const std::string range = request->GetHeaderByName("Range").ToString();
    if (range.empty()) {
      status_ = 200;
    } else if (ParseRange(range, len_, start_, end_)) {
      status_ = 206;
    } else {
      status_ = 416;
    }
    position_ = start_;
    return true;
  }

  void GetResponseHeaders(CefRefPtr<CefResponse> response, int64_t& response_length, CefString&) override {
    response->SetStatus(status_);
    response_length = 0;
    if (status_ == 416) response->SetHeaderByName("Content-Range", "bytes */" + std::to_string(len_), true);
    if (status_ != 200 && status_ != 206) return;
    response->SetMimeType(mime_);
    response->SetHeaderByName("Accept-Ranges", "bytes", true);
    if (status_ == 206) {
      response->SetHeaderByName(
          "Content-Range",
          "bytes " + std::to_string(start_) + "-" + std::to_string(end_ - 1) + "/" + std::to_string(len_), true);
    }
    response_length = static_cast<int64_t>(end_ - start_);
  }

  bool Skip(int64_t bytes_to_skip, int64_t& bytes_skipped, CefRefPtr<CefResourceSkipCallback>) override {
    const uint64_t n = std::min<uint64_t>(static_cast<uint64_t>(bytes_to_skip), end_ - position_);
    position_ += n;
    bytes_skipped = static_cast<int64_t>(n);
    return n > 0;
  }

  bool Read(void* data_out, int bytes_to_read, int& bytes_read, CefRefPtr<CefResourceReadCallback>) override {
    bytes_read = 0;
    if (position_ >= end_) return false;
    const uint32_t want = static_cast<uint32_t>(std::min<uint64_t>(bytes_to_read, end_ - position_));
    const int64_t n = deck_.read(deck_.user, offset_, len_, position_, data_out, want);
    if (n <= 0) {
      bytes_read = ERR_FAILED;
      return false;
    }
    position_ += static_cast<uint64_t>(n);
    bytes_read = static_cast<int>(n);
    return true;
  }

  void Cancel() override {}

 private:
  gd_deck_provider deck_;
  int status_ = 500;
  std::string mime_;
  uint64_t offset_ = 0, len_ = 0, start_ = 0, end_ = 0, position_ = 0;

  IMPLEMENT_REFCOUNTING(DeckResourceHandler);
};

class DeckSchemeFactory : public CefSchemeHandlerFactory {
 public:
  explicit DeckSchemeFactory(gd_deck_provider deck) : deck_(deck) {}

  CefRefPtr<CefResourceHandler> Create(CefRefPtr<CefBrowser>, CefRefPtr<CefFrame>, const CefString&,
                                       CefRefPtr<CefRequest>) override {
    return new DeckResourceHandler(deck_);
  }

 private:
  gd_deck_provider deck_;

  IMPLEMENT_REFCOUNTING(DeckSchemeFactory);
};

// The init script reaches the renderer through the browser's extra_info, and runs before the page's own scripts.
class App : public CefApp, public CefRenderProcessHandler {
 public:
  CefRefPtr<CefRenderProcessHandler> GetRenderProcessHandler() override { return this; }

  // Every process must agree on the scheme, so this runs in the browser and in each helper.
  void OnRegisterCustomSchemes(CefRawPtr<CefSchemeRegistrar> registrar) override {
    registrar->AddCustomScheme(kDeckScheme, CEF_SCHEME_OPTION_STANDARD | CEF_SCHEME_OPTION_SECURE |
                                                CEF_SCHEME_OPTION_CORS_ENABLED | CEF_SCHEME_OPTION_FETCH_ENABLED);
  }

  void OnBrowserCreated(CefRefPtr<CefBrowser>, CefRefPtr<CefDictionaryValue> extra_info) override {
    if (extra_info && extra_info->HasKey(kInitScript)) init_script_ = extra_info->GetString(kInitScript);
  }

  void OnContextCreated(CefRefPtr<CefBrowser>, CefRefPtr<CefFrame> frame, CefRefPtr<CefV8Context> context) override {
    if (!frame->IsMain() || init_script_.empty()) return;
    CefRefPtr<CefV8Value> result;
    CefRefPtr<CefV8Exception> exception;
    if (!context->Eval(init_script_, "glassdeck-init.js", 1, result, exception) && exception) {
      LOG(ERROR) << "init script failed: " << exception->GetMessage().ToString();
    }
  }

  void OnBeforeCommandLineProcessing(const CefString&, CefRefPtr<CefCommandLine> command_line) override {
    // Wallpapers play sound and video without anyone clicking first.
    command_line->AppendSwitchWithValue("autoplay-policy", "no-user-gesture-required");
  }

 private:
  CefString init_script_;
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
// `init_script` may be empty; otherwise it runs in the main frame before the page's scripts.
// `deck` may be null; otherwise it serves deck://wallpaper/ and must outlive gd_web_stop.
int gd_web_start(const char* url, const char* init_script, const gd_deck_provider* deck, uint32_t width,
                 uint32_t height, uint32_t fps, const char* cache_dir, gd_web_frame_fn on_frame, void* user) {
  CefMainArgs args(GetModuleHandleW(nullptr));
  CefSettings settings;
  settings.no_sandbox = true;
  settings.windowless_rendering_enabled = true;
  settings.multi_threaded_message_loop = true;
  settings.log_severity = LOGSEVERITY_WARNING;
  CefString(&settings.root_cache_path).FromString(cache_dir);
  CefRefPtr<App> app(new App);
  if (!CefInitialize(args, settings, app, nullptr)) return 1;
  if (deck && !CefRegisterSchemeHandlerFactory(kDeckScheme, kDeckHost, new DeckSchemeFactory(*deck))) {
    CefShutdown();
    return 3;
  }

  g_client = new Client(static_cast<int>(width), static_cast<int>(height), on_frame, user);
  CefWindowInfo window;
  window.SetAsWindowless(nullptr);
  window.shared_texture_enabled = true;
  CefBrowserSettings browser_settings;
  browser_settings.windowless_frame_rate = static_cast<int>(fps);
  CefRefPtr<CefDictionaryValue> extra_info = CefDictionaryValue::Create();
  extra_info->SetString(kInitScript, CefString(std::string(init_script)));
  if (!CefBrowserHost::CreateBrowser(window, g_client, std::string(url), browser_settings, extra_info, nullptr)) {
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
