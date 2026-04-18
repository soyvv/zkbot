import math


def int_precision(val: float):
    return int(-round(math.log10(float(val)))) if val else 0


def float_precision(val: int):
    return round(0.1 ** val, val) if val else 1.0


def is_close(a, b, precision=12):
    return abs(float(a) - float(b)) < 0.1 ** precision
