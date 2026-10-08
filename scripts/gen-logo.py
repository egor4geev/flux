#!/usr/bin/env python3
"""Логотип flux — «гранёная искра»: шесть лучей-лент разной длины, у каждого светлая и
тёмная грань (сгиб по оси), скруглённые концы, светлая точка в центре. Источник цветного
логотипа `crates/flux-app/assets/brand/logo.svg`:

    python3 scripts/gen-logo.py crates/flux-app/assets/brand/logo.svg

Монохромный силуэт той же геометрии — значок `logo` в scripts/gen-icons.py (менять вместе:
таблица `RAYS`). Иконка приложения строится из цветного логотипа (scripts/app-icon.swift)."""
import math, sys

# Луч: угол (градусы, по часовой от оси x) и длина (из 100). Неровные — как у искры.
RAYS = [(-96, 45), (-37, 35), (14, 42), (70, 33), (122, 44), (178, 36)]
BASE, TIP = 17, 9  # ширина луча у центра и у скруглённого конца

def ray(ang, length, light, dark, c=50, k=1.0):
    ax = (math.cos(math.radians(ang)), math.sin(math.radians(ang)))
    nx = (-ax[1], ax[0])
    p = lambda d, s: (c + ax[0] * d + nx[0] * s, c + ax[1] * d + nx[1] * s)
    f = lambda q: f"{q[0]:.2f} {q[1]:.2f}"
    L, base, r = length * k, BASE * k, TIP * k / 2
    o, te = p(0, 0), p(L, 0)
    # Дуги конца — против часовой (флаг 0): скругление наружу, а не выемка.
    left = f'<path d="M{f(o)} L{f(p(0, base / 2))} L{f(p(L - r, r))} A{r:.2f} {r:.2f} 0 0 0 {f(te)} Z" fill="{light}"/>'
    right = f'<path d="M{f(o)} L{f(te)} A{r:.2f} {r:.2f} 0 0 0 {f(p(L - r, -r))} L{f(p(0, -base / 2))} Z" fill="{dark}"/>'
    return left + right

def logo():
    grad = lambda id, a, b: (f'<linearGradient id="{id}" x1="10" y1="8" x2="90" y2="92" gradientUnits="userSpaceOnUse">'
                             f'<stop offset="0" stop-color="{a}"/><stop offset="1" stop-color="{b}"/></linearGradient>')
    defs = grad("l", "#8fe0ff", "#c2adff") + grad("d", "#4a63ff", "#7b47e6")
    body = "".join(ray(a, length, "url(#l)", "url(#d)") for a, length in RAYS)
    return (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100" width="128" height="128">'
            f'<defs>{defs}</defs>{body}<circle cx="50" cy="50" r="5" fill="#eef0ff"/></svg>\n')

if __name__ == "__main__":
    open(sys.argv[1], "w").write(logo())
