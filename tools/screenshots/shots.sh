# Sourced by tests.sh with SHOTS=1 (tools/screenshots/run.sh), after its setup: makes generated
# pictures with ImageMagick and exiftool (never anyone's own), and saves each view of Glimt as
# /t/out/NAME.png, the names in docs/screenshots. Clicks assume the 1280×800 window of tests.sh.

echo "-- screenshots"
P=/tmp/glimt-demo/Pictures
mkdir -p $P/Summer/City $P/Summer/Coast $P/Summer/Mountains
# A landscape: a sky from $2 to $3, two ridges in $4 and $5, a sun in $6, peaks shifted by $7,
# and a little grain, as a photo has: the same each time, so only views that changed differ.
land() {
    o=$7
    convert -size 1800x1200 gradient:"$2"-"$3" \
        -fill "$4" -draw "polygon 0,700 $((300+o)),430 $((560+o)),640 $((870+o)),330 $((1150+o)),620 $((1450+o)),400 1800,560 1800,1200 0,1200" \
        -fill "$5" -draw "polygon 0,900 $((450+o)),760 $((900+o)),930 $((1350+o)),800 1800,880 1800,1200 0,1200" \
        -fill "$6" -draw "circle $((1300-o)),300 $((1360-o)),300" \
        -seed 1 -attenuate 0.4 +noise Gaussian -quality 90 "$1"
}
cd $P/Summer
land alpine_lake.jpg '#7ec8f0' '#d8f0ff' '#3c7a2a' '#25501c' '#ffffff' 0
land autumn_hills.jpg '#f6c27a' '#ffe9c4' '#c46a1c' '#8a4512' '#fff3c4' 120
land desert_dunes.jpg '#f2b867' '#fde2b0' '#e0913a' '#b8682a' '#fff8e0' -80
land evening_ridge.jpg '#3a2a5e' '#c0607a' '#2a2244' '#1a1530' '#ffd27a' 60
land fjord_sunrise.jpg '#f5ae6a' '#fbd7a8' '#3d5a80' '#293241' '#fff2c4' 0
land glacier_morning.jpg '#a8d8ff' '#f0f8ff' '#c8dcea' '#8aa8c0' '#ffffff' -150
land harbour_dusk.jpg '#1f3a5f' '#e07a5f' '#2b2d42' '#14141f' '#ffd166' 200
land lake_mirror.jpg '#89c2d9' '#e0f4ff' '#2a6f4e' '#1b4332' '#ffffff' -40
land misty_valley.jpg '#b8c4cc' '#eef2f4' '#7d8f99' '#5b6b73' '#f4f4f4' 90
land northern_lights.jpg '#0b1d2e' '#1f6f5c' '#14213d' '#0b0f1e' '#c8ffe0' -120
land red_cliffs.jpg '#f4a261' '#ffe5c2' '#b5452c' '#7a2a1a' '#fff1d0' 30
land summer_meadow.jpg '#8ecae6' '#e8f6ff' '#6a994e' '#386641' '#fffbe0' -60
land sunflower_field.jpg '#9ad1f5' '#e9f7ff' '#e9b824' '#4f772d' '#fffde0' 150
land sunrise_bay.jpg '#ff9e6d' '#ffd6b8' '#4a6fa5' '#1d3557' '#fff4d6' -100
# Near copies, for Find Duplicates by similarity.
convert fjord_sunrise.jpg -resize 1200x800 -modulate 103 fjord_sunrise_small.jpg
convert lake_mirror.jpg -quality 60 lake_mirror_copy.jpg
for f in harbour lights skyline; do land City/$f.jpg '#2b2d42' '#8d99ae' '#1d1e2c' '#0d0e16' '#ffd166' 40; done
for f in beach cove lighthouse; do land Coast/$f.jpg '#90e0ef' '#caf0f8' '#0077b6' '#03045e' '#ffffff' -30; done
for f in peak pass summit; do land Mountains/$f.jpg '#adb5bd' '#f8f9fa' '#495057' '#212529' '#ffffff' 10; done
n=0
for f in *.jpg; do
    n=$((n + 1))
    exiftool -q -overwrite_original -Make=FUJIFILM -Model=X-T5 -LensModel="XF16-55mmF2.8 R LM WR" \
        "-DateTimeOriginal=2025:07:$(printf %02d $(((n - 1) / 4 + 1))) 1$((n % 10)):2$((n % 6)):00" \
        -ISO=$((100 * (n % 4 + 1))) -FNumber=5.6 -ExposureTime=1/$((125 * (n % 3 + 1))) \
        -FocalLength=23 "$f"
done
cd /tmp
printf 'Places|Norway|Lofoten\nPlaces|Norway|Senja\nSeasons|Summer\nSeasons|Autumn\nNature|Mountains\nNature|Sea\n' \
    >$XDG_CONFIG_HOME/glimt/keywords
# The shown image's keywords and comment, set in the tree.
cat >$P/Summer/fjord_sunrise.jpg.xmp <<XMP
<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
 <rdf:Description rdf:about="" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:lr="http://ns.adobe.com/lightroom/1.0/">
  <dc:subject><rdf:Bag><rdf:li>Senja</rdf:li><rdf:li>Summer</rdf:li><rdf:li>Sea</rdf:li></rdf:Bag></dc:subject>
  <dc:description><rdf:Alt><rdf:li xml:lang="x-default">First light over the fjord</rdf:li></rdf:Alt></dc:description>
  <lr:hierarchicalSubject><rdf:Bag><rdf:li>Places|Norway|Senja</rdf:li><rdf:li>Seasons|Summer</rdf:li><rdf:li>Nature|Sea</rdf:li></rdf:Bag></lr:hierarchicalSubject>
 </rdf:Description></rdf:RDF></x:xmpmeta>
XMP
# A Lightroom catalog made on a Mac, of the same pictures.
sqlite3 /tmp/glimt-demo/Lightroom.lrcat <<SQL
CREATE TABLE AgLibraryRootFolder (id_local INTEGER PRIMARY KEY, absolutePath);
CREATE TABLE AgLibraryFolder (id_local INTEGER PRIMARY KEY, rootFolder, pathFromRoot);
CREATE TABLE AgLibraryFile (id_local INTEGER PRIMARY KEY, folder, baseName, extension);
CREATE TABLE Adobe_images (id_local INTEGER PRIMARY KEY, rootFile, rating);
CREATE TABLE AgLibraryCollection (id_local INTEGER PRIMARY KEY, creationId, name, parent);
CREATE TABLE AgLibraryCollectionImage (id_local INTEGER PRIMARY KEY, collection, image);
CREATE TABLE AgLibraryKeyword (id_local INTEGER PRIMARY KEY, name, parent);
INSERT INTO AgLibraryRootFolder VALUES (1, '/Volumes/Photos/');
INSERT INTO AgLibraryFolder VALUES (2, 1, 'Summer/');
INSERT INTO AgLibraryFile VALUES (3, 2, 'fjord_sunrise', 'jpg'), (4, 2, 'lake_mirror', 'jpg'), (5, 2, 'red_cliffs', 'jpg');
INSERT INTO Adobe_images VALUES (6, 3, 5), (7, 4, 4), (8, 5, 3);
INSERT INTO AgLibraryCollection VALUES (9, 'com.adobe.ag.library.group', 'Travel', NULL),
    (10, 'com.adobe.ag.library.collection', 'Norway 2025', 9), (11, 'com.adobe.ag.library.collection', 'Best of', NULL);
INSERT INTO AgLibraryCollectionImage VALUES (12, 10, 6), (13, 10, 7), (14, 11, 6), (15, 11, 8);
INSERT INTO AgLibraryKeyword VALUES (16, NULL, NULL), (17, 'Places', 16), (18, 'Norway', 17), (19, 'Senja', 18);
SQL
# The window, without the rest of the virtual screen.
cshot() { import -window root -crop 1280x800+0+0 +repage /t/out/$1.png; }

config print_per_page=4
start $P/Summer/fjord_sunrise.jpg
sleep 3
cshot main
key ctrl+i; sleep 2; cshot icon-grid; key ctrl+i
key ctrl+t; sleep 1; cshot folder-tree; key ctrl+t
key e; sleep 2; cshot compare; key e
key k; sleep 1; cshot histogram; key k; key k
key ctrl+k; sleep 2
# The tree's branches open from the bottom up, so the rows above stay where they are.
click 1029 526; click 1029 502; click 1047 526; click 1029 478
xdotool mousemove 700 400; sleep 1; cshot keyword-tree; key ctrl+k
key f; sleep 2; cshot full-screen-info; key Escape
# Fuzzy search: "sun" in the field above the file list.
click 120 284; xdotool type sun; sleep 2; cshot fuzzy-search; key ctrl+a; key BackSpace; key Escape
# A contact sheet: the first page of the PDF, as a picture.
answer /tmp/sheet.pdf
key ctrl+a; key ctrl+p; sleep 1; key Return
for i in $(seq 60); do [ -s /tmp/sheet.pdf ] && sleep 2 && break; sleep 1; done
pdftoppm -png -r 50 -f 1 -l 1 -singlefile /tmp/sheet.pdf /t/out/contact-sheet
quit

# Other stars in Glimt than in the catalog, for the import to ask about.
mkdir -p $XDG_STATE_HOME/glimt
printf '0\t3\t%s\n' $P/Summer/fjord_sunrise.jpg >$XDG_STATE_HOME/glimt/marks
start $P/Summer/fjord_sunrise.jpg
sleep 3
answer /tmp/glimt-demo/Lightroom.lrcat
click 20 11; click 100 420 3   # File → Import Lightroom Catalog…
click 650 390; xdotool type /tmp/glimt-demo/Pictures/; sleep 1
click 376 447 2   # Next
xdotool mousemove 700 650; sleep 1
cshot lightroom-import
quit

config layout_names=true
start --layout 2x2 $P/Summer/alpine_lake.jpg
sleep 4
cshot layout
quit

# Without thumbnails, so the names have room beside the size and date.
config file_details=true exif_panel=true thumbnails=false
start $P/Summer/red_cliffs.jpg
sleep 3
cshot exif-and-columns
quit

config
start $P/Summer
sleep 3
# Find Duplicates (D) by similarity, 95 %: only the near copies.
key d; sleep 1; click 402 82; click 372 218; click 505 82 4   # Find, beside the method
xdotool mousemove 900 600; sleep 1
cshot duplicates
quit

config theme=Light
start $P/Summer/lake_mirror.jpg
sleep 3
key ctrl+i; sleep 2; cshot light-theme; key ctrl+i
# The Pan view by the EXIF date, which spreads the pictures over July.
key ctrl+j; sleep 2; click 205 82; sleep 3; cshot pan
click 140 82; sleep 2; cshot calendar; key ctrl+j
click 276 11; click 313 84 2   # Help → Keyboard Map
cshot keyboard-map
quit
