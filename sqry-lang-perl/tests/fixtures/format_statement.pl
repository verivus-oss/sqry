package Grp::FormatStmt;

use strict;

format REPORT =
@<<<<<<<<<<< @>>>>>
$name,      $score
.

sub emit_report {
    my ($name, $score) = @_;
    return write_line($name, $score);
}

sub write_line {
    my ($name, $score) = @_;
    return "$name:$score";
}
