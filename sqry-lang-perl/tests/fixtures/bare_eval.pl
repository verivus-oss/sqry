package Grp::BareEval;

use strict;
use warnings;

sub guarded {
    my $result = eval {
        risky_call();
    };
    return defined $result ? $result : fallback();
}

sub risky_call {
    return 7;
}

sub fallback {
    return 0;
}
