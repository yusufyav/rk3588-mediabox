import sys, numpy as np, tensorflow as tf
from tensorflow.python.framework import tensor_util
rng = np.random.default_rng(0)
x = (rng.random((1, 96, 128, 1)) * 0.3 + np.linspace(0, 0.7, 128)[None, None, :, None]).astype(np.float32)
for name in ('FSRCNN_x2', 'FSRCNN-small_x2', 'ESPCN_x2'):
    g = tf.compat.v1.GraphDef(); g.ParseFromString(open(f'w/{name}.pb', 'rb').read())
    consts = {n.name: tensor_util.MakeNdarray(n.attr['value'].tensor) for n in g.node if n.op == 'Const'}
    with tf.compat.v1.Graph().as_default() as gr:
        tf.compat.v1.import_graph_def(g, name='')
        with tf.compat.v1.Session(graph=gr) as s:
            y = s.run('NCHW_output:0', {'IteratorGetNext:0': x})
    np.savez(f'w/{name}.npz', x=x, y=y, **{k.replace('/', '_'): v for k, v in consts.items()})
    print(name, y.shape, float(y.min()), float(y.max()))
