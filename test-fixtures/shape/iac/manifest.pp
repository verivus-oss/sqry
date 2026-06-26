# Hand-written Puppet manifest sample for body-shape descriptor coverage.
# Puppet definitions are declarative (modelled as Class nodes), but their bodies
# carry real control flow: if/elsif/else and unless branches, a case match, the
# .each iterator, resource declarations, assignment, and a lambda block.
define myapp::service (
  Integer $replicas = 1,
  String  $env,
) {
  $base = $replicas + 1

  if $base > 0 {
    notify { 'positive': }
  } elsif $base < 0 {
    notify { 'negative': }
  } else {
    notify { 'zero': }
  }

  unless $env == 'prod' {
    file { '/tmp/dev-marker':
      ensure => present,
    }
  }

  case $env {
    'prod':    { notify { 'production': } }
    'staging': { notify { 'staging': } }
    default:   { notify { 'unknown': } }
  }

  [1, 2, 3].each |$index| {
    file { "/tmp/replica-${index}":
      ensure => directory,
    }
  }

  service { 'myapp':
    ensure => running,
    enable => true,
  }
}
