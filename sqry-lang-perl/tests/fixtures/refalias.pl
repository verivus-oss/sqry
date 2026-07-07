package Grp::RefAlias;

use strict;
use warnings;
use feature 'refaliasing';
no warnings 'experimental::refaliasing';

my $orig = 41;

sub make_alias {
    my \$alias = \$orig;
    return $alias + 1;
}

sub caller_of_alias {
    return make_alias();
}
