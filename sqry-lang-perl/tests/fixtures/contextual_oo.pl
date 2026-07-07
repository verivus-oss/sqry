use v5.38;
use experimental 'class';

class Shape::Point {
    field $x :param = 0;
    field $y :param = 0;

    method coordinates {
        return summarize($x, $y);
    }
}

role Shape::Drawable {
    method draw {
        return 1;
    }
}

sub summarize {
    my ($x, $y) = @_;
    return "$x,$y";
}
