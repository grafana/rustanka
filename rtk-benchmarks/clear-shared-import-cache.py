"""Release a named prepared-import segment after a benchmark job."""

import ctypes
import hashlib
import os

namespace = os.environ.get("RTK_PREPARED_IMPORT_SHM", "")
if namespace:
    digest = hashlib.sha256(namespace.encode() + os.getuid().to_bytes(4, "little")).digest()
    name = f"/rtk-pi-{digest[:10].hex()}".encode()
    libc = ctypes.CDLL(None, use_errno=True)
    if libc.shm_unlink(name) != 0 and ctypes.get_errno() != 2:
        raise OSError(ctypes.get_errno(), "shm_unlink failed", name)
