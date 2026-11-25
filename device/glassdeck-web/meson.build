project('glassdeck-web', 'c', default_options : ['c_std=gnu11', 'warning_level=2'])

executable('glassdeck-web', 'glassdeck-web.c',
  dependencies : [
    dependency('glib-2.0'),
    dependency('wpe-1.0'),
    dependency('wpebackend-fdo-1.0'),
    dependency('wpe-webkit-2.0'),
  ],
  install : true,
)

install_data('70-glassdeck-web.rules', install_dir : '/etc/udev/rules.d')
