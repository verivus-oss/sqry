package Grp::AsyncBlock;

use strict;
use warnings;
use feature 'async';

sub build_future {
    my $future = async {
        my $value = compute();
        return $value * 2;
    };
    return $future;
}

sub compute {
    return 21;
}
