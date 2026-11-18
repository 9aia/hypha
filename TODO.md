# TODO — hypha

- [ ] T-002 cuora serve cold-start retry: hypha-secret-cuora serve transport now backs off (50ms→1s, 10s cold budget) when the cuora serve child dies before first reply — fixes permanent 'secret unresolved' at service boot. Merged 08ae38d; peregrin dep bumped 7b1c7332. · created:2026-11-18
