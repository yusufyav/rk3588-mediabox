"""MediaBox V2 media core.

The media core is the layer between a content provider and the accepted
RK3588 playback stack. It answers three questions, in this order:

    1. What is this source, exactly?          (media.inspector)
    2. Can this appliance play it as it is?   (media.policy)
    3. If not, what is the cheapest safe fix? (media.proxy)

It has no user interface and no opinion about one. Everything above it talks
to :mod:`media.api` over HTTP and never needs to know that Stremio exists.
"""

__version__ = "2.0.0"
