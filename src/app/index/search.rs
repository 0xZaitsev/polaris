use chumsky::Parser;
use lasso2::{RodeoReader, Spur};
use nohash_hasher::{IntMap, IntSet};
use serde::{Deserialize, Serialize};
use std::{
	collections::{HashMap, HashSet},
	ffi::OsStr,
	path::{Path, PathBuf},
};
use tinyvec::TinyVec;

use crate::app::{
	index::{
		query::{BoolOp, Expr, Literal, NumberField, NumberOp, TextField, TextOp},
		storage::SongKey,
	},
	scanner, Error,
};

use super::{
	query::make_parser,
	storage::{self, sanitize},
};

#[derive(Serialize, Deserialize)]
pub struct Search {
	text_fields: HashMap<TextField, TextFieldIndex>,
	number_fields: HashMap<NumberField, NumberFieldIndex>,
}

impl Default for Search {
	fn default() -> Self {
		Self {
			text_fields: Default::default(),
			number_fields: Default::default(),
		}
	}
}

impl Search {
	pub fn find_songs(
		&self,
		strings: &RodeoReader,
		canon: &HashMap<String, Spur>,
		query: &str,
	) -> Result<Vec<PathBuf>, Error> {
		let parser = make_parser();
		let parsed_query = parser
			.parse(query)
			.map_err(|_| Error::SearchQueryParseError)?;

		let keys = self.eval(strings, canon, &parsed_query);
		Ok(keys
			.into_iter()
			.map(|k| Path::new(OsStr::new(strings.resolve(&k.virtual_path.0))).to_owned())
			.collect::<Vec<_>>())
	}

	fn eval(
		&self,
		strings: &RodeoReader,
		canon: &HashMap<String, Spur>,
		expr: &Expr,
	) -> IntSet<SongKey> {
		match expr {
			Expr::Fuzzy(s) => self.eval_fuzzy(strings, s),
			Expr::TextCmp(field, op, s) => self.eval_text_operator(strings, canon, *field, *op, &s),
			Expr::NumberCmp(field, op, n) => self.eval_number_operator(*field, *op, *n),
			Expr::Combined(e, op, f) => self.combine(strings, canon, e, *op, f),
		}
	}

	fn combine(
		&self,
		strings: &RodeoReader,
		canon: &HashMap<String, Spur>,
		e: &Box<Expr>,
		op: BoolOp,
		f: &Box<Expr>,
	) -> IntSet<SongKey> {
		match op {
			BoolOp::And => self
				.eval(strings, canon, e)
				.intersection(&self.eval(strings, canon, f))
				.cloned()
				.collect(),
			BoolOp::Or => self
				.eval(strings, canon, e)
				.union(&self.eval(strings, canon, f))
				.cloned()
				.collect(),
		}
	}

	fn eval_fuzzy(&self, strings: &RodeoReader, value: &Literal) -> IntSet<SongKey> {
		match value {
			Literal::Text(s) => {
				let mut songs = IntSet::default();
				for field in self.text_fields.values() {
					songs.extend(field.find_like(strings, s));
				}
				songs
			}
			Literal::Number(n) => {
				let mut songs = IntSet::default();
				for field in self.number_fields.values() {
					songs.extend(field.find_equal(*n));
				}
				songs
					.union(&self.eval_fuzzy(strings, &Literal::Text(n.to_string())))
					.copied()
					.collect()
			}
		}
	}

	fn eval_text_operator(
		&self,
		strings: &RodeoReader,
		canon: &HashMap<String, Spur>,
		field: TextField,
		operator: TextOp,
		value: &str,
	) -> IntSet<SongKey> {
		let Some(field_index) = self.text_fields.get(&field) else {
			return IntSet::default();
		};

		match operator {
			TextOp::Eq => field_index.find_exact(canon, value),
			TextOp::Like => field_index.find_like(strings, value),
		}
	}

	fn eval_number_operator(
		&self,
		field: NumberField,
		operator: NumberOp,
		value: i32,
	) -> IntSet<SongKey> {
		todo!()
	}
}

const NGRAM_SIZE: usize = 2;

#[derive(Default, Deserialize, Serialize)]
struct TextFieldIndex {
	exact: HashMap<Spur, IntSet<SongKey>>,
	ngrams: HashMap<[char; NGRAM_SIZE], IntMap<SongKey, Spur>>,
}

impl TextFieldIndex {
	pub fn insert(&mut self, raw_value: &str, value: Spur, key: SongKey) {
		let characters = sanitize(raw_value).chars().collect::<TinyVec<[char; 32]>>();
		for substring in characters[..].windows(NGRAM_SIZE) {
			self.ngrams
				.entry(substring.try_into().unwrap())
				.or_default()
				.insert(key, value);
		}

		self.exact.entry(value).or_default().insert(key);
	}

	pub fn find_like(&self, strings: &RodeoReader, value: &str) -> IntSet<SongKey> {
		let sanitized = sanitize(value);
		let characters = sanitized.chars().collect::<Vec<_>>();
		let empty = IntMap::default();

		let mut candidates = characters[..]
			.windows(NGRAM_SIZE)
			.map(|s| {
				self.ngrams
					.get::<[char; NGRAM_SIZE]>(s.try_into().unwrap())
					.unwrap_or(&empty)
			})
			.collect::<Vec<_>>();

		if candidates.is_empty() {
			return IntSet::default();
		}

		candidates.sort_by_key(|h| h.len());

		candidates[0]
			.iter()
			// [broad phase] Only keep songs that match all bigrams from the search term
			.filter(move |(song_key, _indexed_value)| {
				candidates[1..].iter().all(|c| c.contains_key(&song_key))
			})
			// [narrow phase] Only keep songs that actually contain the search term in full
			.filter(|(_song_key, indexed_value)| {
				let resolved = strings.resolve(indexed_value);
				sanitize(resolved).contains(&sanitized)
			})
			.map(|(k, _v)| k)
			.copied()
			.collect()
	}

	pub fn find_exact(&self, canon: &HashMap<String, Spur>, value: &str) -> IntSet<SongKey> {
		canon
			.get(&sanitize(value))
			.and_then(|s| self.exact.get(&s))
			.cloned()
			.unwrap_or_default()
	}
}

#[derive(Default, Deserialize, Serialize)]
struct NumberFieldIndex {
	values: HashMap<i32, HashSet<SongKey>>,
}

impl NumberFieldIndex {
	pub fn insert(&mut self, raw_value: &str, value: Spur, key: SongKey) {}

	pub fn find_equal(&self, value: i32) -> HashSet<SongKey> {
		todo!()
	}
}

#[derive(Default)]
pub struct Builder {
	text_fields: HashMap<TextField, TextFieldIndex>,
	number_fields: HashMap<NumberField, NumberFieldIndex>,
}

impl Builder {
	pub fn add_song(&mut self, scanner_song: &scanner::Song, storage_song: &storage::Song) {
		let song_key = SongKey {
			virtual_path: storage_song.virtual_path,
		};

		if let (Some(str), Some(spur)) = (&scanner_song.album, storage_song.album) {
			self.text_fields
				.entry(TextField::Album)
				.or_default()
				.insert(str, spur, song_key);
		}

		for (str, spur) in scanner_song
			.album_artists
			.iter()
			.zip(storage_song.album_artists.iter())
		{
			self.text_fields
				.entry(TextField::AlbumArtist)
				.or_default()
				.insert(str, *spur, song_key);
		}

		for (str, spur) in scanner_song.artists.iter().zip(storage_song.artists.iter()) {
			self.text_fields
				.entry(TextField::Artist)
				.or_default()
				.insert(str, *spur, song_key);
		}

		for (str, spur) in scanner_song
			.composers
			.iter()
			.zip(storage_song.composers.iter())
		{
			self.text_fields
				.entry(TextField::Composer)
				.or_default()
				.insert(str, *spur, song_key);
		}

		for (str, spur) in scanner_song.genres.iter().zip(storage_song.genres.iter()) {
			self.text_fields
				.entry(TextField::Genre)
				.or_default()
				.insert(str, *spur, song_key);
		}

		for (str, spur) in scanner_song.labels.iter().zip(storage_song.labels.iter()) {
			self.text_fields
				.entry(TextField::Label)
				.or_default()
				.insert(str, *spur, song_key);
		}

		for (str, spur) in scanner_song
			.lyricists
			.iter()
			.zip(storage_song.lyricists.iter())
		{
			self.text_fields
				.entry(TextField::Lyricist)
				.or_default()
				.insert(str, *spur, song_key);
		}

		self.text_fields.entry(TextField::Path).or_default().insert(
			scanner_song.virtual_path.to_string_lossy().as_ref(),
			storage_song.virtual_path.0,
			song_key,
		);

		if let (Some(str), Some(spur)) = (&scanner_song.title, storage_song.title) {
			self.text_fields
				.entry(TextField::Title)
				.or_default()
				.insert(str, spur, song_key);
		}
	}

	pub fn build(self) -> Search {
		Search {
			text_fields: self.text_fields,
			number_fields: self.number_fields,
		}
	}
}

#[cfg(test)]
mod test {
	use std::path::PathBuf;

	use lasso2::Rodeo;
	use storage::store_song;

	use super::*;

	fn setup_test(songs: Vec<scanner::Song>) -> (Search, RodeoReader, HashMap<String, Spur>) {
		let mut strings = Rodeo::new();
		let mut canon = HashMap::new();

		let mut builder = Builder::default();
		for song in songs {
			let storage_song = store_song(&mut strings, &mut canon, &song).unwrap();
			builder.add_song(&song, &storage_song);
		}

		let search = builder.build();
		let strings = strings.into_reader();
		(search, strings, canon)
	}

	#[test]
	fn can_find_fuzzy() {
		let (search, strings, canon) = setup_test(vec![
			scanner::Song {
				virtual_path: PathBuf::from("seasons.mp3"),
				title: Some("Seasons".to_owned()),
				artists: vec!["Dragonforce".to_owned()],
				..Default::default()
			},
			scanner::Song {
				virtual_path: PathBuf::from("potd.mp3"),
				title: Some("Power of the Dragonflame".to_owned()),
				artists: vec!["Rhapsody".to_owned()],
				..Default::default()
			},
			scanner::Song {
				virtual_path: PathBuf::from("calcium.mp3"),
				title: Some("Calcium".to_owned()),
				artists: vec!["FSOL".to_owned()],
				..Default::default()
			},
		]);

		let songs = search.find_songs(&strings, &canon, "agon").unwrap();

		assert_eq!(songs.len(), 2);
		assert!(songs.contains(&PathBuf::from("seasons.mp3")));
		assert!(songs.contains(&PathBuf::from("potd.mp3")));
	}

	#[test]
	fn can_find_field_like() {
		let (search, strings, canon) = setup_test(vec![
			scanner::Song {
				virtual_path: PathBuf::from("seasons.mp3"),
				title: Some("Seasons".to_owned()),
				artists: vec!["Dragonforce".to_owned()],
				..Default::default()
			},
			scanner::Song {
				virtual_path: PathBuf::from("potd.mp3"),
				title: Some("Power of the Dragonflame".to_owned()),
				artists: vec!["Rhapsody".to_owned()],
				..Default::default()
			},
		]);

		let songs = search
			.find_songs(&strings, &canon, "artist % agon")
			.unwrap();

		assert_eq!(songs.len(), 1);
		assert!(songs.contains(&PathBuf::from("seasons.mp3")));
	}

	#[test]
	fn text_is_case_insensitive() {
		let (search, strings, canon) = setup_test(vec![scanner::Song {
			virtual_path: PathBuf::from("seasons.mp3"),
			artists: vec!["Dragonforce".to_owned()],
			..Default::default()
		}]);

		let songs = search.find_songs(&strings, &canon, "dragonforce").unwrap();
		assert_eq!(songs.len(), 1);
		assert!(songs.contains(&PathBuf::from("seasons.mp3")));

		let songs = search
			.find_songs(&strings, &canon, "artist = dragonforce")
			.unwrap();
		assert_eq!(songs.len(), 1);
		assert!(songs.contains(&PathBuf::from("seasons.mp3")));
	}

	#[test]
	fn can_find_field_exact() {
		let (search, strings, canon) = setup_test(vec![
			scanner::Song {
				virtual_path: PathBuf::from("seasons.mp3"),
				title: Some("Seasons".to_owned()),
				artists: vec!["Dragonforce".to_owned()],
				..Default::default()
			},
			scanner::Song {
				virtual_path: PathBuf::from("potd.mp3"),
				title: Some("Power of the Dragonflame".to_owned()),
				artists: vec!["Rhapsody".to_owned()],
				..Default::default()
			},
		]);

		let songs = search
			.find_songs(&strings, &canon, "artist = Dragon")
			.unwrap();
		assert!(songs.is_empty());

		let songs = search
			.find_songs(&strings, &canon, "artist = Dragonforce")
			.unwrap();
		assert_eq!(songs.len(), 1);
		assert!(songs.contains(&PathBuf::from("seasons.mp3")));
	}

	#[test]
	fn can_use_and_operator() {
		let (search, strings, canon) = setup_test(vec![
			scanner::Song {
				virtual_path: PathBuf::from("whale.mp3"),
				..Default::default()
			},
			scanner::Song {
				virtual_path: PathBuf::from("space.mp3"),
				..Default::default()
			},
			scanner::Song {
				virtual_path: PathBuf::from("whales in space.mp3"),
				..Default::default()
			},
		]);

		let songs = search
			.find_songs(&strings, &canon, "space && whale")
			.unwrap();
		assert_eq!(songs.len(), 1);
		assert!(songs.contains(&PathBuf::from("whales in space.mp3")));

		let songs = search.find_songs(&strings, &canon, "space whale").unwrap();
		assert_eq!(songs.len(), 1);
		assert!(songs.contains(&PathBuf::from("whales in space.mp3")));
	}

	#[test]
	fn can_use_or_operator() {
		let (search, strings, canon) = setup_test(vec![
			scanner::Song {
				virtual_path: PathBuf::from("whale.mp3"),
				..Default::default()
			},
			scanner::Song {
				virtual_path: PathBuf::from("space.mp3"),
				..Default::default()
			},
			scanner::Song {
				virtual_path: PathBuf::from("whales in space.mp3"),
				..Default::default()
			},
		]);

		let songs = search
			.find_songs(&strings, &canon, "space || whale")
			.unwrap();
		assert_eq!(songs.len(), 3);
		assert!(songs.contains(&PathBuf::from("whale.mp3")));
		assert!(songs.contains(&PathBuf::from("space.mp3")));
		assert!(songs.contains(&PathBuf::from("whales in space.mp3")));
	}

	#[test]
	fn avoids_bigram_false_positives() {
		let (search, strings, canon) = setup_test(vec![scanner::Song {
			virtual_path: PathBuf::from("lorry bovine vehicle.mp3"),
			..Default::default()
		}]);

		let songs = search.find_songs(&strings, &canon, "love").unwrap();
		assert!(songs.is_empty());
	}
}
