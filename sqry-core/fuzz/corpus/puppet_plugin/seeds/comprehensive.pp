class apache {
  package { 'httpd':
    ensure => installed,
  }

  service { 'httpd':
    ensure => running,
    enable => true,
  }

  file { '/etc/httpd/conf/httpd.conf':
    ensure  => file,
    content => template('apache/httpd.conf.erb'),
  }
}

define apache::vhost($port, $docroot) {
  file { "/etc/httpd/conf.d/${name}.conf":
    ensure  => file,
    content => template('apache/vhost.conf.erb'),
  }
}
