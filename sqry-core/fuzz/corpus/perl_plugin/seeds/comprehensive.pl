package Math::Calculator;

use strict;
use warnings;

sub new {
    my $class = shift;
    return bless {}, $class;
}

sub add {
    my ($self, $a, $b) = @_;
    return $a + $b;
}

sub multiply {
    my ($self, $a, $b) = @_;
    return $a * $b;
}

1;
