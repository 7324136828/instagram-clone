Third-party software in this build
==================================

The frontend in this folder (index.html and assets/) contains code from the
packages below, minified. Their licence texts are the files next to this one;
they are copies of the LICENSE files the packages are published with.

heic-to                                                        LGPL-3.0
    https://github.com/hoppergee/heic-to
    Converts HEIC/HEIF pictures in the browser. It contains libheif
    (https://github.com/strukturag/libheif) and libde265
    (https://github.com/strukturag/libde265), both LGPL-3.0, compiled to
    JavaScript.

      heic-to.LICENSE.txt   the package's notice and the GNU Lesser General
                            Public License, version 3
      GPL-3.0.txt           the GNU General Public License, version 3, which
                            the LGPL incorporates

    The library is not mixed into the application's own code. It is the
    separate file assets/heic-to-<hash>.js, loaded only when an import
    contains a HEIC/HEIF picture. To use another or a modified version of it,
    change the "heic-to" dependency in frontend/package.json of the project's
    source, run "npm install" and "npm run build" there, and replace the
    contents of this build's frontend folder with the result (see
    "Third-party software" in the project's README.md).

fflate                                                         MIT
    https://github.com/101arrowz/fflate
    Reads compressed data in .zip and .tar.gz exports.

      fflate.LICENSE.txt

react, react-dom, scheduler                                    MIT
    https://github.com/facebook/react
    The user interface.

      react.LICENSE.txt
      react-dom.LICENSE.txt
      scheduler.LICENSE.txt
