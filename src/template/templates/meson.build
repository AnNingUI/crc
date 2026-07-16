project(
  '{{ project_name }}',
  'c',
  version: '{{ version }}',
  default_options: ['c_std=c11', 'warning_level=3'],
)

fs = import('fs')

if not fs.exists('crc/dist/meson.build')
  error('Run crc build before configuring this project')
endif

subdir('crc/dist')

executable(
  '{{ project_name }}',
  files('src/main.c') + cr_generated_sources,
  dependencies: cr_generated_dependencies,
  include_directories: [
    include_directories('include'),
    include_directories('crc/dist/include'),
  ],
)
