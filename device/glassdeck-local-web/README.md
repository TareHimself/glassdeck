# glassdeck-local-web

Renders web wallpapers on the keyboard with WPE WebKit, inside a bubblewrap sandbox of its own
(no network, no root, read-only system, only the MPP decoder, GPU and page folder visible).

Board setup (Debian trixie on RK3566, with MPP and the Rockchip GStreamer plugin installed):

```sh
apt install libwpewebkit-2.0-dev libwpe-1.0-dev libwpebackend-fdo-1.0-dev libjson-glib-dev libgstreamer1.0-dev \
  libgstreamer-plugins-base1.0-dev gstreamer1.0-gl bubblewrap meson ninja-build
meson setup build --prefix=/usr/local && meson test -C build && ninja -C build install
udevadm control --reload && udevadm trigger
useradd --system --no-create-home --shell /usr/sbin/nologin --groups video,render glassdeck-local-web
runuser -u glassdeck-local-web -- glassdeck-local-web --seconds=15 file:///path/to/index.html
# with no receiver running, --display previews straight on the screen
runuser -u glassdeck-local-web -- glassdeck-local-web --display file:///path/to/index.html
# a web background .deck, as the receiver runs it: it spawns this as glassdeck-local-web with its socket end as fd 3
runuser -u glassdeck-local-web -- glassdeck-local-web --compositor-fd=3 --background=/var/lib/glassdeck/packages/<id>.deck
```

Outputs: none (headless test), `--display` (drives the screen; only while no receiver runs), or
`--compositor-fd=N` (the receiver's compositor socket, glassdeck-proto's `compositor` module).
Media inside a background plays through the `backgroundsrc` GStreamer element, installed beside the launcher.
