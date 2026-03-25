# Comprehensive Puppet manifest for testing symbol extraction

class myapp {
  $package_name = 'nginx'
  $service_name = 'nginx'

  package { 'nginx':
    ensure => installed,
  }

  service { 'nginx':
    ensure  => running,
    enable  => true,
    require => Package['nginx'],
  }

  file { '/etc/nginx/nginx.conf':
    ensure  => file,
    content => template('myapp/nginx.conf.erb'),
  }
}

define myapp::config(
  $port = 80,
  $server_name = 'localhost',
) {
  file { "/etc/myapp/${name}.conf":
    ensure  => file,
    content => template('myapp/config.erb'),
  }
}

class webserver {
  include myapp
  require myapp::prereqs

  myapp::config { 'default':
    port        => 8080,
    server_name => 'web.example.com',
  }
}

class database {
  package { 'postgresql':
    ensure => installed,
  }

  service { 'postgresql':
    ensure => running,
    enable => true,
  }
}

node 'web01.example.com' {
  class { 'myapp': }
  include webserver
}
