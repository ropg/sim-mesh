"""Settings every script shares.

The radio below is what the startup script sets every node to, unless it is
tagged `no-radio`. The page reads the first four names, as written here,
for what it draws before anything runs: coverage, links and the loss
tables' band. Keep them plain numbers.
"""

# The channel Berlin's Reticulum LoRa nodes announce (rmap.world: 869.475 MHz,
# 125 kHz, SF7, CR 4/5).
FREQ_MHZ = 869.475
SF = 7
BW_KHZ = 125
CR = 5

# The RNode firmware's sync word, which every RNode-based Reticulum firmware
# has fixed, so every station hears every other.
SYNC = 0x12
