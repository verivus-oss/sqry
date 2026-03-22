package Example::App;

=head1 NAME
Example POD
=cut

use strict;
use warnings;
use Moose;
use List::Util qw(sum max);
use lib 'lib';

require Carp;

our $VERSION = '0.1';

sub foo :lvalue ($$) {
  my ($x, $y) = @_;
  return $x + $y;
}

sub baz ($one = 1, $two = 2) { $one + $two }

method bar :lvalue ($self, $arg) {
  return $arg;
}

has 'name' => (
  is => 'rw',
  default => sub { 'unknown' },
);

my $anon = sub { return 42; };

1;
