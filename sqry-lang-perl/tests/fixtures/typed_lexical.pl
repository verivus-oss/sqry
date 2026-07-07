package Grp::TypedLexical;

use strict;
use warnings;

sub tally {
    my Int $count = 0;
    my Str $label = 'items';
    my Num $ratio = 0.5;
    return accumulate($count, $label, $ratio);
}

sub accumulate {
    my ($count, $label, $ratio) = @_;
    return $count;
}
