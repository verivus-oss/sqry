package Grp::VarGroup;

use strict;
use warnings;

my ($alpha, $beta, $gamma) = (1, 2, 3);
our ($shared_a, $shared_b);
state ($lazy_a, $lazy_b);

sub totals {
    return $alpha + $beta + $gamma;
}

sub reset_group {
    ($alpha, $beta, $gamma) = (0, 0, 0);
    return totals();
}
