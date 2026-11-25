# glassdeck-web

Renders web wallpapers on the keyboard with WPE WebKit, inside a bubblewrap sandbox of its own
(no network, no root, read-only system, only the MPP decoder, GPU and page folder visible).

Board setup (Debian trixie on RK3566, with MPP and the Rockchip GStreamer plugin installed):

```sh
apt install libwpewebkit-2.0-dev libwpe-1.0-dev libwpebackend-fdo-1.0-dev bubblewrap meson ninja-build
meson setup build --prefix=/usr/local && ninja -C build install
udevadm control --reload && udevadm trigger
useradd --system --no-create-home --shell /usr/sbin/nologin --groups video,render glassdeck-web
runuser -u glassdeck-web -- glassdeck-web --seconds=15 file:///path/to/index.html
# with no receiver running, --display previews straight on the screen
runuser -u glassdeck-web -- glassdeck-web --display file:///path/to/index.html
```
