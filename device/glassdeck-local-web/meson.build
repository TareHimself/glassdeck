project('glassdeck-local-web', 'c', default_options : ['c_std=gnu11', 'warning_level=2'])

glib = dependency('glib-2.0')
gio = dependency('gio-2.0')

json = dependency('json-glib-1.0')
gst_plugin_dir = get_option('prefix') / get_option('libdir') / 'glassdeck-local-web' / 'gstreamer'

shared_module('gstdeck', 'gstdecksrc.c', 'deck.c',
  dependencies : [glib, gio, json, dependency('gstreamer-1.0'), dependency('gstreamer-base-1.0')],
  install : true,
  install_dir : gst_plugin_dir,
)

executable('glassdeck-local-web', 'glassdeck-local-web.c', 'compositor.c', 'deck.c',
  c_args : ['-DGST_DECK_PLUGIN_DIR="' + gst_plugin_dir + '"'],
  dependencies : [
    dependency('gbm'),
    glib,
    gio,
    json,
    dependency('libdrm'),
    dependency('wpe-1.0'),
    dependency('wpebackend-fdo-1.0'),
    dependency('wpe-webkit-2.0'),
  ],
  install : true,
)

test('compositor', executable('compositor-test', 'compositor-test.c', 'compositor.c', dependencies : [glib, gio]))

test('deck', executable('deck-test', 'deck-test.c', 'deck.c', dependencies : [glib, gio, json]),
  env : ['G_TEST_SRCDIR=' + meson.current_source_dir()])

install_data('70-glassdeck-local-web.rules', install_dir : '/etc/udev/rules.d')
