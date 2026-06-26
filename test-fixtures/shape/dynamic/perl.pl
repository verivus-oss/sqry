#!/usr/bin/perl
# Hand-written Perl control-flow sample for shape descriptor coverage.
use strict;
use warnings;

sub classify {
    my ($value, @rest) = @_;
    my $result = 0;

    if ($value > 100) {
        $result = 1;
    } elsif ($value > 10) {
        $result = 2;
    } else {
        $result = 3;
    }

    unless ($value) {
        return "empty";
    }

    while ($result > 0) {
        $result--;
        next if $result == 2;
        last if $result < 0;
    }

    for my $i (0 .. 2) {
        helper($i);
    }

    foreach my $entry (@rest) {
        helper($entry);
    }

    my @doubled = map { $_ * 2 } @rest;
    my @kept    = grep { $_ > 0 } @rest;

    my $code = sub { return $_[0] + $result; };

    eval {
        die "bad\n" if $value < 0;
        helper($value);
    };
    if ($@) {
        $result = -1;
    }

    return $result;
}

sub helper {
    my ($x) = @_;
    return $x + 1;
}

1;
